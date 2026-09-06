# COSMIC Files transfer progress

The widget supports live copy/move notifications from a companion COSMIC Files
build. A transfer uses one desktop notification ID from its first progress update
through completion, cancellation, or failure. Active progress remains visible in
the widget even though transient notifications are omitted from COSMIC history.

Stock COSMIC Files at the revision below only sends a generic background-operation
notification after its window closes. It does not publish the per-transfer data
needed by the widget. The companion patch adds that data for **Copy** and **Move**
operations, including transfers started while a Files window remains open.

## Build the companion

The patch targets the installed COSMIC Files 1.8 package's upstream revision:

```text
089ad2b417edded26385029166e5b49c13432bf8
```

In a separate COSMIC Files checkout at that revision:

```bash
git apply /path/to/cosmic-widget-applet/integrations/cosmic-files/transfer-notifications.patch
cargo test --locked --lib operation::transfer_notification::tests
cargo build --locked --release --bin cosmic-files
sudo install -m0755 target/release/cosmic-files /usr/local/bin/cosmic-files
```

Restart COSMIC Files after any active or paused file operations have finished.
Installing under `/usr/local/bin` makes the companion build the default through
`PATH`. Rebase/rebuild the patch when updating to a newer Files version.

## Completion popup timeout

The notification daemon must restart its timer when replacing a notification.
The daemon version used here only scheduled timers for new popups, so replacing
indefinite progress with a completion message left the popup visible forever.
Apply the included `notification-timeouts.patch` to COSMIC Notifications:

```bash
git apply /path/to/cosmic-widget-applet/integrations/cosmic-files/notification-timeouts.patch
cargo test --locked
cargo build --locked --release
sudo install -m0755 target/release/cosmic-notifications /usr/local/bin/cosmic-notifications
```

This patch targets the notification-history branch at
`289ab21628cd7915437316c243c083cbbb2488be`, changes only `src/app.rs`, and preserves
the existing history/action extensions. Replacement timers use generation checks
so old timers cannot expire newer content. Completion uses the daemon's normal
timeout and stays in history after its popup disappears.

The updated daemon is activated at the next login or through a supervised restart
by COSMIC Session. Restarting this daemon clears its in-memory notification history.

## Notification contract

The sender sets app name `COSMIC Files` and desktop entry
`com.system76.CosmicFiles`. The widget only enables transfer handling when both
match and all of the following hints are valid:

| Hint | Value |
| --- | --- |
| `x-cosmic-files-operation` | `copy` or `move` |
| `x-cosmic-files-state` | `running`, `paused`, `completed`, `cancelled`, or `failed` |
| `value` | Signed 32-bit percentage, 0–100 |

Running and paused updates use `transient=true`, no timeout, and suppressed sound.
Changes are sent at most twice per second, with a five-second heartbeat for quiet
or paused transfers so the widget can recover progress after restarting.
Terminal updates use `transient=false` and replace the same notification. The
sender serializes progress and terminal delivery on one connection and notification
ID. Calls target the daemon's unique D-Bus owner; a daemon restart resets the ID.
It waits for terminal delivery before letting the Files operation task finish.
Each call has a two-second timeout, and final delivery gets three attempts, with
close/connection cleanup if they fail. Notification errors do not change
file-operation results.

The widget uses notification daemon owner/ID for row identity, keeps transient
transfers across history refreshes, and preserves dismissals through progress
updates. It removes unfinished rows if their sender disconnects, including
disconnects missed while the monitor reconnects. Active transfers
are excluded from disk cache, so restarting the widget cannot restore stale work.

The companion source patch retains COSMIC Files' GPL-3.0-only license.
