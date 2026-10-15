// SPDX-License-Identifier: MPL-2.0

//! Passive kernel hotplug notifications; waiting replaces an existing worker sleep.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::thread;
use std::time::{Duration, Instant};

const MAX_EVENTS_PER_WAIT: usize = 64;
const SETTLE_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Changes {
    pub(super) topology: bool,
    pub(super) power: bool,
}

impl Changes {
    pub(super) fn is_empty(self) -> bool {
        !self.topology && !self.power
    }
}

pub(super) struct Listener {
    socket: Option<OwnedFd>,
}

impl Listener {
    pub(super) fn new() -> io::Result<Self> {
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: socket returned a new descriptor, whose ownership is transferred here.
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut address: libc::sockaddr_nl = unsafe { mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_pid = 0; // Let the kernel assign a unique port for each listener.
        address.nl_groups = 1; // Kernel uevents, not userspace rebroadcasts.
        let bound = unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_nl).cast(),
                mem::size_of_val(&address) as libc::socklen_t,
            )
        };
        if bound < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            socket: Some(socket),
        })
    }

    pub(super) fn wait(&mut self, timeout: Duration) -> Changes {
        let started = Instant::now();
        let nonblocking = timeout.is_zero();
        let mut deadline = timeout;
        let mut changes = Changes::default();
        let Some(fd) = self.socket.as_ref().map(AsRawFd::as_raw_fd) else {
            thread::sleep(timeout);
            return changes;
        };
        let mut received = 0;
        let mut payload = [0_u8; 16 * 1024];
        // A zero timeout still drains already queued messages after a worker
        // overruns its polling deadline; it never enters the settling wait.
        while nonblocking || started.elapsed() < deadline {
            let remaining = deadline.saturating_sub(started.elapsed());
            let millis = remaining
                .as_millis()
                .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0));
            let mut descriptor = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready =
                unsafe { libc::poll(&mut descriptor, 1, millis.min(i32::MAX as u128) as i32) };
            if ready < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    if nonblocking {
                        return changes;
                    }
                    continue;
                }
                self.disable_until(started, timeout);
                return changes;
            }
            if ready == 0 {
                if nonblocking {
                    return changes;
                }
                continue;
            }
            if descriptor.revents & (libc::POLLHUP | libc::POLLNVAL) != 0 {
                self.disable_until(started, timeout);
                return changes;
            }
            let mut pending_error = descriptor.revents & libc::POLLERR != 0;
            while received < MAX_EVENTS_PER_WAIT && (nonblocking || started.elapsed() < deadline) {
                received += 1;
                let mut sender: libc::sockaddr_nl = unsafe { mem::zeroed() };
                let mut sender_length = mem::size_of_val(&sender) as libc::socklen_t;
                // MSG_TRUNC returns the full datagram length, allowing oversized
                // messages to be rejected rather than parsing an incomplete event.
                let length = unsafe {
                    libc::recvfrom(
                        fd,
                        payload.as_mut_ptr().cast(),
                        payload.len(),
                        libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                        (&mut sender as *mut libc::sockaddr_nl).cast(),
                        &mut sender_length,
                    )
                };
                let incoming = if length < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if error.kind() == io::ErrorKind::WouldBlock {
                        if pending_error {
                            self.disable_until(started, timeout);
                            return changes;
                        }
                        break;
                    }
                    if error.raw_os_error() == Some(libc::ENOBUFS) {
                        pending_error = false;
                        // Lost notifications require refreshing both kinds of state.
                        Changes {
                            topology: true,
                            power: true,
                        }
                    } else {
                        self.disable_until(started, timeout);
                        return changes;
                    }
                } else {
                    pending_error = false;
                    if length as usize > payload.len()
                        || sender_length as usize != mem::size_of_val(&sender)
                        || sender.nl_family != libc::AF_NETLINK as libc::sa_family_t
                        || sender.nl_pid != 0
                    {
                        continue;
                    }
                    parse_event(&payload[..length as usize])
                };
                if !incoming.is_empty() {
                    if changes.is_empty() {
                        // A USB connection emits several related events. Coalesce
                        // them without extending the original worker deadline.
                        deadline = timeout.min(started.elapsed().saturating_add(SETTLE_DELAY));
                    }
                    changes.topology |= incoming.topology;
                    changes.power |= incoming.power;
                }
            }
            if nonblocking {
                return changes;
            }
            if received == MAX_EVENTS_PER_WAIT {
                // An unrelated event storm must not turn the worker into a busy loop.
                sleep_remaining(started, deadline);
                return changes;
            }
        }
        changes
    }

    fn disable_until(&mut self, started: Instant, timeout: Duration) {
        self.socket = None;
        sleep_remaining(started, timeout);
    }
}

fn sleep_remaining(started: Instant, timeout: Duration) {
    thread::sleep(timeout.saturating_sub(started.elapsed()));
}

fn parse_event(payload: &[u8]) -> Changes {
    if payload.last() != Some(&0) {
        return Changes::default();
    }
    let mut action = None;
    let mut subsystem = None;
    for field in payload.split(|byte| *byte == 0) {
        if let Some(value) = field.strip_prefix(b"ACTION=") {
            action = Some(value);
        } else if let Some(value) = field.strip_prefix(b"SUBSYSTEM=") {
            subsystem = Some(value);
        }
    }
    if !matches!(
        action,
        Some(b"add" | b"remove" | b"change" | b"bind" | b"unbind")
    ) {
        return Changes::default();
    }
    Changes {
        topology: matches!(subsystem, Some(b"usb" | b"hidraw" | b"sound")),
        power: subsystem == Some(b"power_supply".as_slice()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Changes, Listener, parse_event};
    use std::fs::File;
    use std::os::unix::net::UnixDatagram;
    use std::time::{Duration, Instant};

    #[test]
    fn recognizes_hotplug_and_charging_events() {
        for action in ["add", "remove", "change", "bind", "unbind"] {
            for subsystem in ["usb", "hidraw", "sound", "power_supply"] {
                let event =
                    format!("{action}@/devices/test\0ACTION={action}\0SUBSYSTEM={subsystem}\0");
                assert_eq!(
                    parse_event(event.as_bytes()),
                    Changes {
                        topology: subsystem != "power_supply",
                        power: subsystem == "power_supply",
                    }
                );
            }
        }
    }

    #[test]
    fn ignores_unrelated_and_incomplete_messages() {
        for event in [
            b"ACTION=change\0SUBSYSTEM=thermal\0".as_slice(),
            b"ACTION=online\0SUBSYSTEM=usb\0",
            b"ACTION=add\0SUBSYSTEM=hidraw",
            b"SUBSYSTEM=power_supply\0",
            b"ACTION=add\0",
            b"",
        ] {
            assert!(parse_event(event).is_empty());
        }
    }

    #[test]
    fn rejects_non_kernel_senders_and_preserves_wait_deadline() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        sender.send(b"ACTION=add\0SUBSYSTEM=hidraw\0").unwrap();
        let mut listener = Listener {
            socket: Some(receiver.into()),
        };
        let timeout = Duration::from_millis(5);
        let started = Instant::now();
        assert!(listener.wait(timeout).is_empty());
        assert!(started.elapsed() >= timeout);
    }

    #[test]
    fn zero_timeout_drains_queued_messages_without_waiting() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let inspection = receiver.try_clone().unwrap();
        sender.send(b"ACTION=add\0SUBSYSTEM=hidraw\0").unwrap();
        let mut listener = Listener {
            socket: Some(receiver.into()),
        };
        let started = Instant::now();
        // This is a well-formed event from a non-kernel sender: consume it but
        // never report it as a device change.
        assert!(listener.wait(Duration::ZERO).is_empty());
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(
            inspection.recv(&mut [0; 64]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
        );
        assert!(listener.socket.is_some());
        assert!(listener.wait(Duration::ZERO).is_empty());
    }

    #[test]
    fn failed_socket_falls_back_to_sleep() {
        let mut listener = Listener {
            socket: Some(File::open("/dev/null").unwrap().into()),
        };
        let timeout = Duration::from_millis(5);
        let started = Instant::now();
        assert!(listener.wait(timeout).is_empty());
        assert!(started.elapsed() >= timeout);
        assert!(listener.socket.is_none());
        let started = Instant::now();
        assert!(listener.wait(timeout).is_empty());
        assert!(started.elapsed() >= timeout);
    }

    #[test]
    #[ignore = "requires a Linux kernel uevent socket"]
    fn opens_and_waits_for_kernel_events() {
        let mut listener = Listener::new().expect("kernel uevent listener unavailable");
        let timeout = Duration::from_millis(50);
        let started = Instant::now();
        let changes = listener.wait(timeout);
        assert!(started.elapsed() >= timeout);
        assert!(listener.socket.is_some());
        println!("Kernel battery events: {changes:?}");
    }
}
