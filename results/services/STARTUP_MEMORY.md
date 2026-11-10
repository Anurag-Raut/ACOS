# ServiceHost startup cost and heap (launch set)

`cargo run -q -p acos-services --example measure_host`; peak RSS from `/proc/self/status` (`VmHWM`), since `/usr/bin/time` is not assumed.

```text
ServiceHost::standard() install: 91924 ns
lazily construct 9 services:   375911 ns
100 replayed checkPermission calls: 1497986 ns (14979 ns/call)
VmRSS:  3000 kB -> 7636 kB
VmSize: 7896 kB -> 9188 kB
VmHWM:  7636 kB
constructions: 9
registered descriptors: 19
registered service names: 19
```

The launch set is the nine `IServiceManager.checkService` names the framework asks for. `ServiceHost::standard()` parses no package data; the PMS parses `hello-views.apk` on its first request.
