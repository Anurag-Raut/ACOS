# NET6 checkpoint 1 (diagnosis in progress)

Bug: after `offline`, a second fetch in the same guest process never returns.

Measured on real ART (`run/stacks6/`), with the fixture's stack watchdog and a
live `SIGQUIT`-style probe:

* the second fetch's DNS request **never reaches** `/dev/socket/dnsproxyd`
  (`run/stacks6/dns.log` has one line, the first fetch's; `ss -tnp` shows **no
  TCP socket** for the guest at all, so it is not in `connect(2)`).
* the `net-check-fetch` thread is in `ppoll(fds, 1, <finite timeout>, NULL)`
  (x86_64 syscall 271, wchan `poll_schedule_timeout`), i.e. a *timed* poll on one
  descriptor that does not return.
* the watchdog's `Thread.getAllStackTraces()` dump never appears in logcat,
  i.e. the app's log path is not a reliable witness here; the `run/` evidence
  uses `/proc` and `ss` instead.

Next: identify the descriptor being polled (read the `pollfd` out of the guest's
`/proc/<pid>/mem` and resolve the socket inode) and take a SIGQUIT ART stack from
the guest's stderr fd.
