#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Check the real justfile in isolated fixtures; never build or use real sudo."""

import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[1]
RESOURCES = [
    'resources/app.desktop', 'resources/widget.desktop', 'resources/settings.desktop',
    'resources/app.metainfo.xml', 'resources/icon.svg',
    'resources/70-cosmic-widget-headsets.rules', 'assets/weather/meteocons/LICENSE',
    'assets/icons/material-design/LICENSE',
]
BINARIES = ['cosmic-widget-applet', 'cosmic-widget-iced', 'cosmic-widget-settings']
INSTALLED_NAMES = ['cosmic-widget-applet', 'cosmic-widget', 'cosmic-widget-settings']

FAKE_CARGO = '''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
assert args[:3] == ['build', '--locked', '--release'], args
assert os.geteuid() != 0
with open(os.environ['INSTALL_TEST_LOG'], 'a') as stream:
    stream.write(json.dumps({'tool': 'cargo', 'args': args}) + '\\n')
if os.environ.get('INSTALL_TEST_FAIL') == '1':
    sys.exit(17)
target = pathlib.Path(args[args.index('--target-dir') + 1]) / 'release'
target.mkdir(parents=True, exist_ok=True)
bins = [args[index + 1] for index, value in enumerate(args) if value == '--bin']
assert bins == ['cosmic-widget-applet', 'cosmic-widget-iced', 'cosmic-widget-settings']
if os.environ.get('INSTALL_TEST_OMIT') == '1':
    bins.pop()
for name in bins:
    path = target / name
    path.write_text('fresh build\\n')
    path.chmod(0o755)
'''

FAKE_SUDO = '''#!/usr/bin/env python3
import json, os, pathlib, pwd, subprocess, sys
args = sys.argv[1:]
assert args[:2] == ['-H', '-u'], args
assert pwd.getpwnam(args[2]).pw_uid == os.getuid() != 0, args
assert args[3:6] == ['--', 'sh', '-c'], args
assert args[6] == 'PATH="$HOME/.cargo/bin:$PATH"; export PATH; exec cargo "$@"', args
assert args[7] == 'sh', args
with open(os.environ['INSTALL_TEST_LOG'], 'a') as stream:
    stream.write(json.dumps({'tool': 'sudo', 'user': args[2]}) + '\\n')
sys.exit(subprocess.run([str(pathlib.Path(__file__).with_name('cargo')), *args[8:]]).returncode)
'''


def check_installer(root):
    commands = root / 'commands'
    commands.mkdir()
    for name, content in [('cargo', FAKE_CARGO), ('sudo', FAKE_SUDO)]:
        command = commands / name
        command.write_text(content)
        command.chmod(0o755)
    source = (REPO / 'justfile').read_text()
    # Only the root condition is replaced; the real caller-selection logic runs.
    # The sudo stub validates delegation without switching users or invoking a shell.
    root_condition = 'if [[ "$EUID" == 0 ]]; then'
    assert source.count(root_condition) == 1
    source = source.replace(root_condition, 'if [[ "${INSTALL_TEST_ROOT:-0}" == 1 ]]; then')

    def case(name, *, existing=False, prebuilt=False, missing_resource=False,
             fail=False, omit=False, root_mode=False, sudo_user=None,
             pkexec_uid=None, ok=True):
        fixture = root / name
        fixture.mkdir()
        (fixture / 'justfile').write_text(source)
        for resource in RESOURCES:
            destination = fixture / resource
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(REPO / resource, destination)
        if missing_resource:
            (fixture / RESOURCES[-1]).unlink()
        output = fixture / "output dir's $ literal"
        release = output / 'release'
        release.mkdir(parents=True)
        if existing:
            for binary in BINARIES:
                path = release / binary
                path.write_text('old build\n')
                path.chmod(0o755)
        stage = fixture / 'staged root'
        log = fixture / 'calls.jsonl'
        env = os.environ.copy()
        env.update(PATH=str(commands) + os.pathsep + env['PATH'],
                   INSTALL_TEST_LOG=str(log), INSTALL_TEST_ROOT=str(int(root_mode)),
                   INSTALL_TEST_FAIL=str(int(fail)), INSTALL_TEST_OMIT=str(int(omit)))
        env.pop('SUDO_USER', None)
        env.pop('PKEXEC_UID', None)
        if sudo_user is not None:
            env['SUDO_USER'] = sudo_user
        if pkexec_uid is not None:
            env['PKEXEC_UID'] = pkexec_uid
        command = ['just', '--justfile', str(fixture / 'justfile'),
                   f'cargo-target-dir={output}', f'rootdir={stage}',
                   'install-prebuilt' if prebuilt else 'install']
        result = subprocess.run(command, env=env, text=True, capture_output=True)
        assert (result.returncode == 0) == ok, (name, result.stdout, result.stderr)
        calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        files = [path for path in stage.rglob('*') if path.is_file()]
        if ok:
            expected = 'old build\n' if prebuilt else 'fresh build\n'
            for binary in INSTALLED_NAMES:
                installed = stage / 'usr/local/bin' / binary
                assert installed.read_text() == expected, (name, binary)
                assert installed.stat().st_mode & 0o777 == 0o755
            assert len(files) == len(RESOURCES) + len(BINARIES), (name, files)
            expected_calls = [] if prebuilt else ['sudo', 'cargo'] if root_mode else ['cargo']
            assert [entry['tool'] for entry in calls] == expected_calls, (name, calls)
        else:
            assert not files, (name, files)
            if not fail and not omit:
                assert not calls, (name, calls)
        print('PASS', name)

    username = pwd.getpwuid(os.getuid()).pw_name
    case('build-missing')
    case('refresh-existing', existing=True)
    case('failed-build-before-copy', existing=True, fail=True, ok=False)
    case('missing-resource-before-build', missing_resource=True, ok=False)
    case('incomplete-build-before-copy', omit=True, ok=False)
    case('prebuilt-all-files', existing=True, prebuilt=True)
    case('prebuilt-direct-root', existing=True, prebuilt=True, root_mode=True)
    case('prebuilt-missing-binary', prebuilt=True, ok=False)
    case('prebuilt-missing-resource', existing=True, prebuilt=True, missing_resource=True, ok=False)
    case('sudo-caller', existing=True, root_mode=True, sudo_user=username)
    case('pkexec-caller', existing=True, root_mode=True, pkexec_uid=str(os.getuid()))
    case('direct-root-rejected', existing=True, root_mode=True, ok=False)
    case('root-sudo-caller-rejected', existing=True, root_mode=True, sudo_user='root', ok=False)
    case('unknown-sudo-caller-rejected', existing=True, root_mode=True,
         sudo_user='cosmic-widget-user-does-not-exist', ok=False)
    case('root-pkexec-caller-rejected', existing=True, root_mode=True, pkexec_uid='0', ok=False)
    case('invalid-pkexec-caller-rejected', existing=True, root_mode=True, pkexec_uid='invalid', ok=False)


if __name__ == '__main__':
    if os.geteuid() == 0:
        raise SystemExit('Run this check as a normal user; it never needs root.')
    with tempfile.TemporaryDirectory(prefix='cosmic-widget-installer-check-') as directory:
        check_installer(Path(directory))
