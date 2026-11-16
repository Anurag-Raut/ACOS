# SPIKE-WV gate summary (generated)

Engine: HeadlessChrome/157.0.8103.78 (@4b3d0dabb8250b6f152d2d85d79e7706e309d087)

| gate | item | verdict | numbers |
|---|---|---|---|
| a | real HTTPS page loads | **GO** | https://example.com/ -> complete nodes=22 load=335.1ms; https://www.iana.org/help/example-domains -> complete nodes=114 load=514.6ms |
| b | JS bridge round trip both ways | **GO** | host->page='ACOS WebView probe'; page->host=['tapped']; async='delayed-ok' |
| c | tap on web button by text (a11y tree) | **GO** | ax role=button bounds={'x': 54, 'y': 173.4, 'w': 260, 'h': 70.0} state=tapped real-page-nav=True |
| d | frame time < 50 ms (HeadlessExperimental.beginFrame, PNG) | **GO** | 1080x800@2.625 median=2.84ms p95=3.08ms max=3.49ms; 1080x2400@2.625 median=8.0ms p95=8.38ms max=8.99ms |
| e | extra memory per additional session (WebView) <= 60 MiB | **NO-GO** | PSS 32.0 MiB/session, RSS 138.0 MiB/session |

Verdicts: 4/5 GO
