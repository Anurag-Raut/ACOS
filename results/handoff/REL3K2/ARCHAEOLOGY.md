# REL3K2 item 3 — which commit changed the launch ladder reply

## 1. The reply the test reads
```
$ git log --oneline -8 -- crates/acos-cli/tests/native_launch.rs
432e8c09 Audit round 2 fixes: GLES resource budgets and GLSL step budget, private MCP session root, bounded caches, host-guard wall check, honest fixture gates, STATUS provenance notes
27c725e1 Native: AssetManager mObject fix, Java-level NDK round trip (asset, bitmap, surface frame), MCP confined-child native sessions
fbf7e4f1 APK native code under acos launch --native: System.loadLibrary bridge, native-hello and hostile fixtures, MCP native option

$ git log --oneline -3 -- crates/acos-cli/src/launch.rs
2ee3e3ad Merge branch 'rel3b' (provenance + strict inputs + review fixes) into rel-030
87cb6908 REL3B: verdict provenance + validated inputs (scenario/APK identity, closed tool schemas, one engine vocabulary)
73a2dbdd REL3F: one front door — acos guide, session list text, positional session ids, human selectors, stream contract
```
The ladder builder (`ladder_json`) and the test are older than the regression; neither changed in the window that broke it.

## 2. Where the ladder is lost
The CLI prints `--json` through `print_json -> prepare_reply -> reply::enforce_cap_pretty` (crates/acos-cli/src/main.rs). Before the cap existed, the whole envelope (ladder included) was printed.

```
$ git log -S"pub fn enforce_cap_pretty" --oneline -- crates/acos-protocol/src/reply.rs
41566095 REL3C: one compact, capped reply rule for MCP and CLI JSON

$ git show --stat 41566095 | head -6
commit 41566095745a8ae300b7c5273d82df9ea6ce90b9
Author: Anurag Raut <rautab0@gmail.com>
Date:   Mon Nov 16 08:15:32 2026 +0530

    REL3C: one compact, capped reply rule for MCP and CLI JSON
    

$ git show 41566095^:crates/acos-protocol/src/reply.rs >/dev/null 2>&1; echo "exit=$?"
exit=128  (non-zero: the cap module did not exist before 41566095)

$ git show 41566095 -- crates/acos-cli/src/main.rs | grep -E "^\+.*(enforce_cap|prepare_reply)" | head -5
+    let value = prepare_reply(envelope);
+fn prepare_reply<T: Serialize>(envelope: &T) -> Value {
+    acos_protocol::reply::enforce_cap_pretty(&mut value, acos_protocol::reply::MAX_REPLY_BYTES);
```

## 3. Mechanism (measured on rel-030, before the fix)
`acos launch --json fixtures/build/apps/native-hello.apk` (no `--native`) returned the launch ladder of 72,292 bytes: the same 13,463-byte blocker text on 5 rungs (`application_on_create` blocked, the other four `not_reached`), because the Rust VM's native-call error carries the java-tail and native-tail frame lists. `enforce_cap_measured` step 3 then replaced the *whole* `result` with `{reply_capped, cap_bytes, note}`, so `result.stages` and `result.reached_stage` disappeared and the test read `<missing>`.

## 4. Fix
`crates/acos-protocol/src/reply.rs`: before replacing the result, bound every string longer than `CAP_STRING_BYTES` (head + line-aligned tail + marker naming the byte counts and the artifacts) and replace a repeat of an already-bounded string with a back-reference that keeps its first line. The per-string budget halves until the reply fits or hits `CAP_STRING_FLOOR`. Structure survives; only prose shrinks. The wholesale replacement remains for a result that cannot be shrunk (many small strings, e.g. an unpaged tree), which is what that step is for.

Same reply after the fix: `result.stages` has all 8 rungs, `reached_stage: bind_application`, `stages[3].blocker.code = UNSUPPORTED_JAVA_API`, the message keeps the `--native` hint, and the whole reply is under 20 KiB (see native-launch-pass-after.log).
