# IPDict Final Review Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three load-bearing findings left by the final EncIPDict review, then repeat full automated and Windows 5.8.6 acceptance testing.

**Architecture:** Keep the working IP2/EncIPDict pipeline and add compare-and-commit trust semantics around the peer-key cache, collision-resistant payload-state inheritance for classic messages, and retry-safe seen handling after classic decryption errors. Each change is isolated behind a focused state/net helper and covered by a RED/GREEN regression before final integration verification.

**Tech Stack:** Rust 2021, Tokio, `rsa` 0.9, SHA-256, Tauri v2, Vue 3, Node test runner.

**Spec:** `docs/superpowers/specs/2026-09-03-official-ipdict-receive-design.md`, plus the user-approved automatic-unseal behavior in Task 7 of `docs/superpowers/plans/2026-09-03-official-ipdict-receive.md`

## Global Constraints

- Work on `codex/official-ipdict-receive` in `/ssd/open_ipmsg/.worktrees/official-ipdict-receive`; do not modify the main checkout.
- Preserve the verified full-IP2, exact 64-NUL retry, raw typed getter, full-packet signature, persistence-before-ACK, strict FILE, and auto-unseal behavior.
- Existing cached peer trust wins over packet-supplied keys; first-contact TOFU must be atomic.
- A different payload must never inherit `read`, `unlocked`, or file runtime state merely because it reuses a PKT.
- Classic decrypt failure must leave the same PKT retryable after rehandshake.
- Do not log BODY, complete ciphertext, keys, or attachment content.
- Use TDD and keep test/check output free of new warnings.
- Start live testing with `WEBKIT_DISABLE_DMABUF_RENDERER=1`.

---

### Task 1: Atomic Verified Peer-Key Compare-and-Commit

**Files:**
- Modify: `src-tauri/src/state.rs:639-735`
- Modify: `src-tauri/src/net.rs:2790-2870`
- Test: inline tests in `state.rs` and `net.rs`

**Interfaces:**
- Produces `VerifiedPeerKeyDecision::{TofuStored, Current, Previous, Mismatch}`.
- Produces `AppState::commit_verified_peer_key(ip, capa, embedded) -> VerifiedPeerKeyDecision`, with the complete decision and optional mutation under one `peer_crypto` lock.
- Consumes the existing cryptographic verification result; it does not perform RSA verification while holding the mutex.

- [ ] **Step 1: Write concurrent TOFU and stale-snapshot tests**

Add a barrier-based test where two threads commit different verified RSA-2048 keys for an empty IP. Assert exactly one returns `TofuStored`, the other returns `Mismatch`, and the stored current key is the winner. Add a stale-snapshot test: current key rotates between signature verification and commit; committing the former current may return `Previous` but must not rotate it back.

- [ ] **Step 2: Run focused tests and verify RED**

Run the new tests with full module-qualified names and `--exact`. Expected: compilation failure because the decision API does not exist, or both TOFU writes are accepted by the current snapshot/update sequence.

- [ ] **Step 3: Implement compare-and-commit under one cache lock**

Use this decision matrix while holding `peer_crypto` once:

```rust
match entry {
    None => insert embedded as current and return TofuStored,
    Some(e) if e.pub_key == embedded => update capa only and return Current,
    Some(e) if e.prev_key.as_ref() == Some(embedded) => return Previous,
    Some(_) => return Mismatch,
}
```

Do not change current/previous on `Previous` or `Mismatch`.

- [ ] **Step 4: Integrate the atomic decision after signature verification**

The receive flow may snapshot cached candidates for RSA verification, but must call `commit_verified_peer_key` before peer/history/dedup/emit/download/ACK side effects. `Mismatch` triggers `crypto_rehandshake` and returns. `TofuStored`/`Current`/`Previous` continue; only `TofuStored` and `Current` may update capability state through this API.

- [ ] **Step 5: Verify security paths**

Run focused tests for concurrent TOFU, self-signed replacement, current key, previous key, and no-side-effects mismatch; then run all state/net tests, `cargo check`, and `git diff --check`.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/state.rs src-tauri/src/net.rs
git commit -m "fix(security): make EncIPDict trust commit atomic"
```

---

### Task 2: Bind Classic State Inheritance to an Exact Payload Identity

**Files:**
- Modify: `src-tauri/src/net.rs:1580-1740`
- Test: inline tests in `net.rs`

**Interfaces:**
- Produces an end-anchored `strip_official_delayed_suffix` helper.
- Produces a classic payload identity that includes command-relevant flags, normalized BODY, and complete attachment identity including raw/file ID.
- Existing verified EncIPDict SHA-256 `payload_id` remains unchanged.

- [ ] **Step 1: Write collision regressions**

Add focused tests proving:

```text
BODY containing "IPMsg Delayed Send" in the middle != BODY without the tail
same filename/size/mtime/attr but different file ID != same identity
same PKT + different PASSWORDOPT payload cannot inherit unlocked/read/file state
```

Retain a positive test: the exact official trailing delayed footer is removed so a genuine retry keeps the same identity.

- [ ] **Step 2: Run focused tests and verify RED**

Expected: marker-in-body and file-ID collision assertions fail with the current identity helper.

- [ ] **Step 3: Implement exact delayed-footer recognition**

Only remove a suffix at the end of BODY with this complete official shape:

```text
\n----\n(IPMsg Delayed Send: <non-empty timestamp> )
```

Do not truncate a marker elsewhere, a missing closing `)`, an empty timestamp, or trailing bytes after `)`.

- [ ] **Step 4: Include all attachment identity fields**

Hash the file raw ID (or canonical numeric ID plus original raw form), name, size, mtime, attr, and ordered extension attributes. Preserve file ordering and use explicit domain separators/lengths.

- [ ] **Step 5: Gate inherited state on exact identity equality**

Read `prev_unlocked`, `already_read`, and prior file runtime state only if the previous record's `payload_id` equals the current identity. An identity conflict starts `read=false`; PASSWORDOPT starts `locked=true/unlocked=false`.

- [ ] **Step 6: Verify and commit**

Run collision, delayed retry, password, file, all net, cargo check, and diff checks, then:

```bash
git add src-tauri/src/net.rs
git commit -m "fix(protocol): bind classic state to exact payload"
```

---

### Task 3: Release Seen State After Classic Decryption Failure

**Files:**
- Modify: `src-tauri/src/net.rs:1030-1300`
- Test: inline async tests in `net.rs`

**Interfaces:**
- Consumes existing `mark_seen` / `forget_seen`.
- Produces retry-safe classic encrypted SENDMSG handling.
- Closes the deferred minor by emitting `users-updated` only when trusted metadata changes.

- [ ] **Step 1: Write the decrypt-retry regression**

Send classic `SENDMSG | ENCRYPTOPT | SENDCHECKOPT` with fixed PKT and invalid encrypted extra. Establish/refresh the correct key and resend a valid encrypted packet with the same PKT. Assert the second attempt decrypts, persists/emits once, and receives ACK. Before the fix, it is dropped because the first attempt retained seen.

- [ ] **Step 2: Write the metadata notification regression**

For an existing peer, deliver trusted EncIPDict with changed NCK/GRP/CVER/STAT. Assert `users-updated` is emitted only when fields change.

- [ ] **Step 3: Run focused tests and verify RED**

Expected: same-PKT valid retry is dropped; metadata changes do not notify the frontend.

- [ ] **Step 4: Release seen on every pre-persistence classic failure**

Call `forget_seen(from.ip(), pkt.pkt_no)` before returning from classic decryption failure. Audit SENDMSG early returns after `mark_seen`; any failure before persistent success must release the reservation. Do not release after successful persistence.

- [ ] **Step 5: Emit metadata refresh only on a real change**

Compare trusted metadata while holding the peer lock, mutate fields, save a `changed` bool, release the lock, then emit `users-updated` once if true. Never emit while holding the mutex.

- [ ] **Step 6: Verify and commit**

Run focused, classic encryption, metadata, all net/state, full cargo, cargo check and diff checks, then:

```bash
git add src-tauri/src/net.rs
git commit -m "fix(protocol): allow classic retry after decrypt failure"
```

---

### Task 4: Final Automated and Windows Verification

**Files:**
- Verify the complete branch; no planned production edit.

- [ ] **Step 1: Run fresh fail-fast automation**

```bash
set -o pipefail
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
pnpm test
pnpm build
cargo run --manifest-path src-tauri/Cargo.toml -- --selftest
git diff --check
```

Every command must exit 0 with no new warning or FAIL line.

- [ ] **Step 2: Run a fresh broad review**

Review the complete branch from merge base, focused on Tasks 1-3. Any Important finding blocks merge.

- [ ] **Step 3: Start the feature client**

```bash
WEBKIT_DISABLE_DMABUF_RENDERER=1 pnpm tauri dev -- -- --log
```

Verify exactly one UDP 2425 listener comes from this worktree.

- [ ] **Step 4: Repeat Windows 5.8.6 acceptance**

With encryption/IPDict enabled and IPv6 multicast disabled, send a normal sealed message and numeric message. Verify display, signature trust, persistent identity, same-PKT RECVMSG, READMSG after visible-chat marking, and Windows queue clearance.

- [ ] **Step 5: Report honestly**

List exact automated counts, review verdict, and Windows observations. Mark any unexecuted check “未验证”.
