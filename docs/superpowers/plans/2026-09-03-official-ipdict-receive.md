# Official IPDict / EncIPDict Receive Compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Rust client receive, authenticate, display, and acknowledge official Windows IP Messenger 5.8.6 `IP2` / EncIPDict messages, including delayed retries with the official 64-byte NUL suffix.

**Architecture:** Port the observable semantics of the official 4.99r3 `IPDict`, `ResolveMsg`, `DecIPDict`, and `ResolveDictMsg` flow while retaining the existing Rust module boundaries. `IPDict` stores raw ordered values and decodes them only through typed getters; the UDP entry point recognizes a complete `IP2` envelope before classic packets, validates the retry suffix, decrypts and verifies EncIPDict, resolves it to the existing `protocol::Packet`, dispatches it through the existing SENDMSG path, then acknowledges the inner packet number.

**Tech Stack:** Rust 2021, Tokio UDP, `rsa` 0.9, `aes` 0.8, `ctr` 0.9, Tauri v2, Vue 3, Node test runner.

**Spec:** `docs/superpowers/specs/2026-09-03-official-ipdict-receive-design.md`

## Global Constraints

- The public reference is `shirouzu/ipmsg` commit `733f2515b34f7a5f84342448540b1a61d9f1dd0b` (4.99r3); do not claim source-level parity with closed/unpublished 5.8.6 code.
- Validate final behavior against real Windows IP Messenger 5.8.6 traffic.
- Preserve classic `1:packet:user:host:command:extra` behavior and existing DIR/member-master behavior.
- Treat a recognized malformed or partial `IP2` packet as invalid; never reinterpret it as a classic packet.
- Accept only an exact 64-byte all-NUL suffix after a complete `IP2` packet as the official delayed-retry checksum workaround.
- Do not log decrypted BODY, session keys, private keys, or complete attachments.
- Keep changes inside the existing module structure; do not move unrelated code or format the whole Rust tree.
- Use TDD for every production behavior: observe the focused test fail for the intended reason before implementing it.
- The working tree currently contains two known experimental diffs in `src-tauri/src/ipdict.rs` and `src-tauri/src/net.rs`; restore only those two files to `HEAD` before Task 1, after saving their diff for audit.

---

## File Structure

- Modify `src-tauri/src/ipdict.rs`: ordered raw-value storage, official pack/unpack delimiters, typed getters, nested dict/list parsing, and unit tests.
- Modify `src-tauri/src/crypto.rs`: raw-byte EI/EK/EB access, official full-IPDict signing and verification helpers, signed EncIPDict fixture creation, and crypto tests.
- Modify `src-tauri/src/net.rs`: strict `IP2` envelope classification, unified IPDict/EncIPDict receive path, typed ResolveDictMsg conversion, signature/key handling, deduplication, ACK ordering, and focused network tests.
- Modify `src-tauri/src/selftest.rs`: official full-`IP2` end-to-end scenarios for unpadded sends, 64-NUL retries, numeric BODY, secret semantics, duplicate delivery, and real ACK capture.
- Modify `README.md`: correct the EncIPDict wire-format statement and document delayed retry compatibility.

No new production module is needed. `ipdict.rs` remains the wire-format boundary, `crypto.rs` remains the cryptographic boundary, and `net.rs` remains orchestration/dispatch.

---

### Task 0: Restore the Confirmed Source Baseline

**Files:**
- Restore: `src-tauri/src/ipdict.rs`
- Restore: `src-tauri/src/net.rs`
- Preserve: `docs/superpowers/specs/2026-09-03-official-ipdict-receive-design.md`

**Interfaces:**
- Consumes: the known experimental working-tree patch created during diagnosis.
- Produces: a clean source baseline at commit `427ceef` before new TDD work starts.

- [ ] **Step 1: Audit and save the known experimental diff**

Run:

```bash
git status --short
git diff -- src-tauri/src/ipdict.rs src-tauri/src/net.rs \
  > /tmp/open-ipmsg-pre-official-ipdict.patch
git diff --name-only
```

Expected: only `src-tauri/src/ipdict.rs` and `src-tauri/src/net.rs` are modified; the saved patch contains the temporary BODY special-case/test and verbose EncIPDict candidate logging.

- [ ] **Step 2: Restore only the two known experimental files**

Run:

```bash
git restore --source=HEAD --worktree -- \
  src-tauri/src/ipdict.rs src-tauri/src/net.rs
git status --short
```

Expected: no source modification remains. Do not use `git reset --hard`, do not restore any other path, and do not delete the design or plan documents.

---

### Task 1: Port Official Raw-Value IPDict Semantics

**Files:**
- Modify: `src-tauri/src/ipdict.rs:73-379`
- Modify call sites: `src-tauri/src/crypto.rs:150-178`
- Modify call sites: `src-tauri/src/net.rs:2446-2493`
- Test: `src-tauri/src/ipdict.rs` inline `#[cfg(test)]` module

**Interfaces:**
- Consumes: official `IPDict::set`, `pack_content`, `unpack_core`, and typed `get_*` semantics.
- Produces:
  - `Dict { pub items: Vec<(String, Vec<u8>)> }`
  - `Dict::get(&self, key: &str) -> Option<&[u8]>`
  - `Dict::get_bytes(&self, key: &str) -> Option<&[u8]>`
  - `Dict::get_int(&self, key: &str) -> Option<i64>`
  - `Dict::get_str(&self, key: &str) -> Option<&str>`
  - `Dict::get_dict(&self, key: &str) -> Option<Dict>`
  - `Dict::get_dict_list(&self, key: &str) -> Vec<Dict>`
  - `Dict::pack_prefix(&self, max_items: usize) -> Vec<u8>` for signature verification
  - existing `put_*`, `pack`, `unpack`, and `pack_content` entry points with official wire output.

- [ ] **Step 1: Write failing getter and byte-preservation tests**

Replace direct `Val`-shape assertions with getter assertions and add these tests:

```rust
#[test]
fn unpack_keeps_values_raw_until_the_requested_getter() {
    let wire = b"IP2:20:VER:1:3:BODY:7:1111111:BIN:3:\x00:\xff:Z";
    let (d, used) = Dict::unpack(wire).expect("official IPDict");

    assert_eq!(used, wire.len());
    assert_eq!(d.get_int("VER"), Some(3));
    assert_eq!(d.get_str("BODY"), Some("1111111"));
    assert_eq!(d.get_int("BODY"), Some(0x1111111));
    assert_eq!(d.get_bytes("BIN"), Some(&b"\x00:\xff"[..]));
    assert_eq!(d.pack(), wire);
}

#[test]
fn empty_text_is_a_valid_string_value() {
    let wire = Dict::new().put_str(DICT_BODY, "").pack();
    let (d, used) = Dict::unpack(&wire).expect("empty BODY");

    assert_eq!(used, wire.len());
    assert_eq!(d.get_str(DICT_BODY), Some(""));
    assert_eq!(d.get_bytes(DICT_BODY), Some(&b""[..]));
}

#[test]
fn official_dict_list_uses_colons_between_items() {
    let mut a = Dict::new();
    a.put_str("UID", "a");
    let mut b = Dict::new();
    b.put_str("UID", "b");
    let wire = Dict::new().put_dict_list("LIST", &[a, b]).pack();
    let (d, _) = Dict::unpack(&wire).expect("dict list");

    let list = d.get_dict_list("LIST");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].get_str("UID"), Some("a"));
    assert_eq!(list[1].get_str("UID"), Some("b"));
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml \
  ipdict::tests::unpack_keeps_values_raw_until_the_requested_getter -- --exact --nocapture
cargo test --manifest-path src-tauri/Cargo.toml \
  ipdict::tests::empty_text_is_a_valid_string_value -- --exact --nocapture
cargo test --manifest-path src-tauri/Cargo.toml \
  ipdict::tests::official_dict_list_uses_colons_between_items -- --exact --nocapture
```

Expected: the first test fails because `BODY` is eagerly converted to `Int`; the second fails because an empty raw value becomes an empty Dict; the third fails because the current list packer omits official inter-item colons or the eager parser chooses the wrong type.

- [ ] **Step 3: Replace eager `Val` inference with ordered raw buffers**

Use one storage form for every field:

```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Dict {
    pub items: Vec<(String, Vec<u8>)>,
}

impl Dict {
    fn set_raw(&mut self, key: &str, value: Vec<u8>) -> &mut Self {
        if let Some((_, current)) = self.items.iter_mut().find(|(k, _)| k == key) {
            *current = value;
        } else {
            self.items.push((key.to_string(), value));
        }
        self
    }

    pub fn put_int(&mut self, key: &str, value: i64) -> &mut Self {
        let raw = if value < 0 {
            format!("-{:x}", value.unsigned_abs()).into_bytes()
        } else {
            format!("{value:x}").into_bytes()
        };
        self.set_raw(key, raw)
    }

    pub fn put_str(&mut self, key: &str, value: &str) -> &mut Self {
        self.set_raw(key, value.as_bytes().to_vec())
    }

    pub fn put_bytes(&mut self, key: &str, value: &[u8]) -> &mut Self {
        self.set_raw(key, value.to_vec())
    }

    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.items
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_slice())
    }

    pub fn get_bytes(&self, key: &str) -> Option<&[u8]> {
        self.get(key)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        std::str::from_utf8(self.get(key)?).ok()
    }
}
```

Implement `get_int` against the full raw slice, including the official optional leading `-`; reject empty, sign-only, more than 16 hex digits, or non-hex bytes. Do not trim BODY or other string values.

- [ ] **Step 4: Port official dict/list packing and parsing delimiters**

Use a colon between adjacent dict entries and between adjacent list entries:

```rust
fn pack_content_prefix(&self, max_items: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, (key, value)) in self.items.iter().take(max_items).enumerate() {
        if index > 0 {
            out.push(b':');
        }
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(format!(":{:x}:", value.len()).as_bytes());
        out.extend_from_slice(value);
    }
    out
}

pub fn put_dict_list(&mut self, key: &str, list: &[Dict]) -> &mut Self {
    let mut raw = Vec::new();
    for (index, dict) in list.iter().enumerate() {
        if index > 0 {
            raw.push(b':');
        }
        let item = dict.pack_content_prefix(dict.items.len());
        raw.extend_from_slice(format!("{:x}:", item.len()).as_bytes());
        raw.extend_from_slice(&item);
    }
    self.set_raw(key, raw)
}
```

`parse_content` must insert each exact value slice through `set_raw`; it must not call `parse_val`. `get_dict` calls exact content parsing on the stored raw bytes. `get_dict_list` parses repeated `hex_len:value` items with a required colon between adjacent items, then parses every item as dict content. Remove `Val`, `parse_val`, and their direct-shape tests once no caller remains.

- [ ] **Step 5: Add exact prefix packing for signatures**

Implement full packet packing over an explicit item prefix:

```rust
pub fn pack_prefix(&self, max_items: usize) -> Vec<u8> {
    let content = self.pack_content_prefix(max_items.min(self.items.len()));
    let mut out = format!("IP2:{:x}:", content.len()).into_bytes();
    out.extend_from_slice(&content);
    out.extend_from_slice(b":Z");
    out
}

pub fn pack(&self) -> Vec<u8> {
    self.pack_prefix(self.items.len())
}
```

Keep `pub fn pack_content(d: &Dict) -> Vec<u8>` as a compatibility wrapper over all items. `unpack_content` must parse exact content only; remove the incorrect `1:<packet>:...:Z` and trailing-NUL behavior from that helper.

- [ ] **Step 6: Migrate byte consumers without changing their behavior**

In `crypto::open_encipdict`, replace `Val::Bytes` matches with typed access:

```rust
let iv = d.get_bytes(ipdict::DICT_ENCIV).ok_or("缺 EI")?.to_vec();
if iv.len() != 16 {
    return Err("EI 不是 16 字节 IV".into());
}
let ek = d.get_bytes(ipdict::DICT_ENCKEY).ok_or("缺 EK")?.to_vec();
let eb = d.get_bytes(ipdict::DICT_ENCBODY).ok_or("缺 EB")?.to_vec();
```

In `net::dict_verify`, read `SIGN` and `PUBN` through `get_bytes`; retain the current signature algorithm until Task 2 changes its signed bytes.

- [ ] **Step 7: Run focused and module tests and verify GREEN**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml ipdict::tests -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml crypto::encipdict_tests -- --nocapture
cargo check --manifest-path src-tauri/Cargo.toml
git diff --check
```

Expected: all IPDict and EncIPDict unit tests pass, compilation succeeds, and there are no whitespace errors.

- [ ] **Step 8: Commit the raw-value model**

```bash
git add src-tauri/src/ipdict.rs src-tauri/src/crypto.rs src-tauri/src/net.rs
git commit -m "refactor(protocol): port official IPDict value semantics"
```

---

### Task 2: Align Full-IPDict Signing and Verification

**Files:**
- Modify: `src-tauri/src/crypto.rs:118-225,1285-1364`
- Modify: `src-tauri/src/net.rs:2446-2493`
- Test: `src-tauri/src/crypto.rs` inline tests

**Interfaces:**
- Consumes: `Dict::pack()` and `Dict::pack_prefix(max_items)` from Task 1.
- Produces:
  - `crypto::sign_ipdict(dict: &mut Dict, key: &KeyPair, capa: u32) -> Result<(), String>`
  - `crypto::verify_ipdict(dict: &Dict) -> Result<Option<(RsaPublicKey, u32)>, String>`
  - `seal_encipdict` that encrypts a signed inner full IPDict.

- [ ] **Step 1: Write failing full-packet signature tests**

Add tests that distinguish official full-packet signing from the current content-only signing:

```rust
#[test]
fn ipdict_signature_covers_full_packet_except_final_sign() {
    let key = KeyPair::generate().unwrap();
    let mut d = crate::ipdict::Dict::new();
    d.put_int(crate::ipdict::DICT_VER, 3)
        .put_int(crate::ipdict::DICT_PKT, 42)
        .put_str(crate::ipdict::DICT_UID, "sender")
        .put_str(crate::ipdict::DICT_HID, "host")
        .put_int(crate::ipdict::DICT_CMD, 0x20)
        .put_int(crate::ipdict::DICT_FLG, 0)
        .put_str(crate::ipdict::DICT_BODY, "1111111");

    sign_ipdict(&mut d, &key, CAPA_OUR_SEND).unwrap();
    let verified = verify_ipdict(&d).unwrap().expect("signed");
    assert_eq!(verified.0.n(), key.public_key().n());
    assert_eq!(verified.1, CAPA_OUR_SEND);

    d.put_str(crate::ipdict::DICT_BODY, "1111112");
    assert!(verify_ipdict(&d).is_err());
}

#[test]
fn ipdict_signature_must_be_the_last_field() {
    let key = KeyPair::generate().unwrap();
    let mut d = crate::ipdict::Dict::new();
    d.put_int(crate::ipdict::DICT_VER, 3);
    sign_ipdict(&mut d, &key, CAPA_OUR_SEND).unwrap();
    d.put_str("AFTER", "not-signed");

    assert!(verify_ipdict(&d).is_err());
}
```

Import `rsa::traits::PublicKeyParts` inside the test module.

- [ ] **Step 2: Run the signature tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml \
  crypto::encipdict_tests::ipdict_signature_covers_full_packet_except_final_sign \
  -- --exact --nocapture
cargo test --manifest-path src-tauri/Cargo.toml \
  crypto::encipdict_tests::ipdict_signature_must_be_the_last_field \
  -- --exact --nocapture
```

Expected: compilation fails because `sign_ipdict` / `verify_ipdict` do not exist, or assertions fail because the current net helpers hash content-only bytes.

- [ ] **Step 3: Implement official signing in `crypto.rs`**

Add the official field order and hash the full `IP2` packet before appending SIGN:

```rust
pub fn sign_ipdict(
    dict: &mut crate::ipdict::Dict,
    key: &KeyPair,
    capa: u32,
) -> Result<(), String> {
    use crate::ipdict::*;
    dict.items.retain(|(name, _)| name != DICT_SIGN);
    dict.put_int(DICT_PUBE, key.public_exponent() as i64)
        .put_bytes(DICT_PUBN, &key.modulus_be())
        .put_int(DICT_EF, DICT_EF_SHA256)
        .put_int(DICT_EC, capa as i64);
    let signature = key.sign_sha256(&dict.pack())?;
    dict.put_bytes(DICT_SIGN, &signature);
    Ok(())
}
```

This must use `dict.pack()`, not `pack_content(dict)`.

- [ ] **Step 4: Implement official verification in `crypto.rs`**

Require SIGN to be last, rebuild the full prefix packet, and return the verified embedded public key and capability:

```rust
pub fn verify_ipdict(
    dict: &crate::ipdict::Dict,
) -> Result<Option<(RsaPublicKey, u32)>, String> {
    use crate::ipdict::*;
    use rsa::BigUint;

    let Some(sign) = dict.get_bytes(DICT_SIGN) else {
        return Ok(None);
    };
    if dict.items.last().map(|(key, _)| key.as_str()) != Some(DICT_SIGN) {
        return Err("SIGN 不是末尾字段".into());
    }
    let ef = dict.get_int(DICT_EF).ok_or("缺 EF")? as u32;
    if ef & DICT_EF_SHA256 as u32 == 0 {
        return Err("SIGN 未声明 SHA-256".into());
    }
    let capa = dict.get_int(DICT_EC).ok_or("缺 EC")? as u32;
    let exponent = dict.get_int(DICT_PUBE).ok_or("缺 PUBE")?;
    let modulus = dict.get_bytes(DICT_PUBN).ok_or("缺 PUBN")?;
    let public = RsaPublicKey::new(
        BigUint::from_bytes_be(modulus),
        BigUint::from(exponent as u64),
    )
    .map_err(|e| format!("IPDict 公钥无效：{e}"))?;
    let signed = dict.pack_prefix(dict.items.len() - 1);
    if !verify_sha256(&public, &signed, sign) {
        return Err("IPDict SHA-256 签名校验失败".into());
    }
    Ok(Some((public, capa)))
}
```

Reject non-positive exponents before converting to `u64`.

- [ ] **Step 5: Reuse the crypto helpers from DIR and EncIPDict fixture code**

Replace `net::dict_sign` internals with:

```rust
let capa = entry_caps(&ctx.st.config()) | crypto::CAPA_OUR_SEND;
crypto::sign_ipdict(d, &ctx.st.own_keypair(), capa)
```

Replace `net::dict_verify` internals with a wrapper over `crypto::verify_ipdict`. Update `seal_encipdict` to clone and sign the inner dict before encrypting:

```rust
let mut signed_inner = inner.clone();
sign_ipdict(&mut signed_inner, me, CAPA_OUR_SEND)?;
let plain = signed_inner.pack();
```

Remove `let _ = me;`. Update comments that still describe the outer wire as `1:<packet>:...`.

- [ ] **Step 6: Run crypto, DIR, and compile verification**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml crypto::encipdict_tests -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml net::tests -- --nocapture
cargo check --manifest-path src-tauri/Cargo.toml
git diff --check
```

Expected: signatures pass only over the full prefix IPDict, tampering fails, and existing DIR tests still pass after both signer and verifier adopt the official byte sequence.

- [ ] **Step 7: Commit signing alignment**

```bash
git add src-tauri/src/crypto.rs src-tauri/src/net.rs
git commit -m "fix(protocol): align official IPDict signatures"
```

---

### Task 3: Add a Strict Official `IP2` Datagram Classifier

**Files:**
- Modify: `src-tauri/src/net.rs:820-910,3200-3400`
- Test: `src-tauri/src/net.rs` inline `#[cfg(test)]` module

**Interfaces:**
- Consumes: `Dict::unpack(data) -> Option<(Dict, usize)>` from Task 1.
- Produces: `parse_ipdict_datagram(data: &[u8]) -> Result<Option<(Dict, usize)>, String>` where `usize` is accepted suffix length (`0` or `64`).

- [ ] **Step 1: Write failing classifier tests**

Add focused tests:

```rust
#[test]
fn ipdict_datagram_accepts_exact_official_retry_padding() {
    let mut d = crate::ipdict::Dict::new();
    d.put_int(crate::ipdict::DICT_EF, crate::ipdict::ENCIPDICT_EF);
    let wire = d.pack();

    let (_, pad0) = super::parse_ipdict_datagram(&wire).unwrap().unwrap();
    assert_eq!(pad0, 0);

    let mut retry = wire;
    retry.extend_from_slice(&[0; 64]);
    let (_, pad64) = super::parse_ipdict_datagram(&retry).unwrap().unwrap();
    assert_eq!(pad64, 64);
}

#[test]
fn ipdict_datagram_rejects_partial_or_wrong_suffix_without_classic_fallback() {
    let wire = crate::ipdict::Dict::new().put_int("A", 1).pack();
    for suffix in [&[0u8; 63][..], &[0u8; 65][..], &[1u8][..]] {
        let mut bad = wire.clone();
        bad.extend_from_slice(suffix);
        assert!(super::parse_ipdict_datagram(&bad).is_err());
    }
    assert!(super::parse_ipdict_datagram(b"IP2:5:bad:Z").is_err());
}

#[test]
fn ipdict_datagram_leaves_classic_packets_untouched() {
    let classic = b"1:42:user:host:32:hello";
    assert!(super::parse_ipdict_datagram(classic).unwrap().is_none());
    assert_eq!(crate::protocol::parse(classic).unwrap().extra, b"hello");
}
```

- [ ] **Step 2: Run classifier tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml \
  net::tests::ipdict_datagram_accepts_exact_official_retry_padding -- --exact --nocapture
cargo test --manifest-path src-tauri/Cargo.toml \
  net::tests::ipdict_datagram_rejects_partial_or_wrong_suffix_without_classic_fallback \
  -- --exact --nocapture
cargo test --manifest-path src-tauri/Cargo.toml \
  net::tests::ipdict_datagram_leaves_classic_packets_untouched -- --exact --nocapture
```

Expected: compilation fails because the classifier is absent.

- [ ] **Step 3: Implement the pure classifier**

Place this helper immediately before `handle_datagram`:

```rust
fn parse_ipdict_datagram(
    data: &[u8],
) -> Result<Option<(crate::ipdict::Dict, usize)>, String> {
    if !data.starts_with(crate::ipdict::IPDICT_HEAD.as_bytes()) {
        return Ok(None);
    }
    let (dict, used) = crate::ipdict::Dict::unpack(data)
        .ok_or_else(|| "IP2 外壳或内容长度无效".to_string())?;
    let suffix = &data[used..];
    if suffix.is_empty() {
        return Ok(Some((dict, 0)));
    }
    if suffix.len() == 64 && suffix.iter().all(|byte| *byte == 0) {
        return Ok(Some((dict, 64)));
    }
    Err(format!("IP2 非法尾随数据：{}B", suffix.len()))
}
```

Do not integrate it into `handle_datagram` yet; Task 4 performs that behavior change together with the complete EncIPDict resolver.

- [ ] **Step 4: Run classifier and existing parser tests and verify GREEN**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml ipdict_datagram_ -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml protocol::tests::packet_roundtrip -- --exact
cargo check --manifest-path src-tauri/Cargo.toml
git diff --check
```

Expected: all classifier tests pass; classic parser behavior remains unchanged.

- [ ] **Step 5: Commit the classifier**

```bash
git add src-tauri/src/net.rs
git commit -m "test(protocol): define strict IP2 datagram boundary"
```

---

### Task 4: Integrate Official ResolveDictMsg, EncIPDict Dispatch, and ACK Ordering

**Files:**
- Modify: `src-tauri/src/net.rs:820-1135,2536-2674`
- Test: `src-tauri/src/net.rs` inline tests

**Interfaces:**
- Consumes:
  - `parse_ipdict_datagram` from Task 3
  - `crypto::open_encipdict`
  - `crypto::verify_ipdict`
  - existing `handle_sendmsg` and `handle_dict_datagram`.
- Produces:
  - `resolve_ipdict_packet(dict: &Dict) -> Result<protocol::Packet, String>`
  - `handle_encipdict(ctx: &NetCtx, outer: &Dict, from: SocketAddr)` with no fake outer packet number
  - accepted-message ACK after persistence; duplicate-message ACK immediately.

- [ ] **Step 1: Write failing ResolveDictMsg tests**

Add tests for required fields, raw BODY, and flag preservation:

```rust
fn official_sendmsg_dict(body: &str, flags: u32) -> crate::ipdict::Dict {
    let mut d = crate::ipdict::Dict::new();
    d.put_int(crate::ipdict::DICT_VER, 3)
        .put_int(crate::ipdict::DICT_PKT, 665500)
        .put_str(crate::ipdict::DICT_UID, "sender")
        .put_str(crate::ipdict::DICT_HID, "win-host")
        .put_int(crate::ipdict::DICT_CMD, crate::protocol::cmd::SENDMSG as i64)
        .put_int(crate::ipdict::DICT_FLG, flags as i64)
        .put_str(crate::ipdict::DICT_BODY, body);
    d
}

#[test]
fn resolve_ipdict_packet_preserves_numeric_body_and_flags() {
    let flags = crate::protocol::opt::SENDCHECKOPT
        | crate::protocol::opt::SECRETOPT
        | crate::protocol::opt::ENCRYPTOPT
        | crate::protocol::opt::UTF8OPT;
    let wire = official_sendmsg_dict("1111111", flags).pack();
    let (parsed, _) = crate::ipdict::Dict::unpack(&wire).unwrap();
    let packet = super::resolve_ipdict_packet(&parsed).unwrap();

    assert_eq!(packet.pkt_no, 665500);
    assert_eq!(packet.extra, b"1111111");
    assert_eq!(packet.command, crate::protocol::cmd::SENDMSG | flags);
}

#[test]
fn resolve_ipdict_packet_rejects_missing_required_fields() {
    for missing in ["VER", "PKT", "UID", "HID", "CMD", "FLG"] {
        let mut d = official_sendmsg_dict("body", 0);
        d.items.retain(|(key, _)| key != missing);
        assert!(super::resolve_ipdict_packet(&d).is_err(), "missing {missing}");
    }
}
```

- [ ] **Step 2: Run ResolveDictMsg tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml \
  net::tests::resolve_ipdict_packet_preserves_numeric_body_and_flags -- --exact --nocapture
cargo test --manifest-path src-tauri/Cargo.toml \
  net::tests::resolve_ipdict_packet_rejects_missing_required_fields -- --exact --nocapture
```

Expected: compilation fails because `resolve_ipdict_packet` does not exist; the old inline handler also strips SECRET/ENCRYPT and defaults missing FLG.

- [ ] **Step 3: Implement typed ResolveDictMsg conversion**

Extract the existing BODY/FILE reconstruction into a pure resolver and make all official required fields mandatory:

```rust
fn resolve_ipdict_packet(d: &crate::ipdict::Dict) -> Result<proto::Packet, String> {
    if d.get_int(ipd::DICT_VER) != Some(3) {
        return Err("VER 不是 IPMSG_NEW_VERSION(3)".into());
    }
    let pkt_no = u32::try_from(d.get_int(ipd::DICT_PKT).ok_or("缺 PKT")?)
        .map_err(|_| "PKT 超出 u32")?;
    let user = d.get_str(ipd::DICT_UID).ok_or("缺 UID")?.to_string();
    let host = d.get_str(ipd::DICT_HID).ok_or("缺 HID")?.to_string();
    let mode = u32::try_from(d.get_int(ipd::DICT_CMD).ok_or("缺 CMD")?)
        .map_err(|_| "CMD 超出 u32")?;
    let flags = u32::try_from(d.get_int(ipd::DICT_FLG).ok_or("缺 FLG")?)
        .map_err(|_| "FLG 超出 u32")?;
    let body = d.get_str(ipd::DICT_BODY).unwrap_or("");
    let mut extra = body.as_bytes().to_vec();

    let files = resolve_ipdict_files(d)?;
    if !files.is_empty() {
        extra.push(0);
        let encoded: Vec<String> = files.iter().map(|file| file.serialize("utf8")).collect();
        extra.extend_from_slice(encoded.join("\u{7}").as_bytes());
        extra.push(0x07);
    }
    Ok(proto::Packet { pkt_no, user, host, command: mode | flags, extra })
}
```

Extract `resolve_ipdict_files` from the current inline closure. Return `Err` for a FILE item missing `FI` or `FN`; keep official defaults only for optional FS/MT/FA/CP fields. Do not clear `SECRETOPT` or `ENCRYPTOPT`.

- [ ] **Step 4: Replace both ad-hoc envelope branches at `handle_datagram`**

The top of `handle_datagram` must use one authoritative decision:

```rust
match parse_ipdict_datagram(data) {
    Ok(Some((dict, padding))) => {
        ctx.st.diag(&format!(
            "<- {from} IP2 len={} padding={}B keys={}",
            data.len(), padding, dict.items.len()
        ));
        if dict.has(ipd::DICT_ENCBODY) {
            handle_encipdict(ctx, &dict, from).await;
        } else {
            handle_dict_datagram(ctx, &dict, from).await;
        }
        return;
    }
    Err(error) => {
        ctx.st.diag(&format!("<- {from} IP2 拒绝：{error}"));
        return;
    }
    Ok(None) => {}
}
let Some(pkt) = proto::parse(data) else { return };
```

Delete the old exact-length `IP2` branch and the incorrect `1:<packet>:EF...` branch. Do not include raw ciphertext hex in the permanent log.

- [ ] **Step 5: Rewrite EncIPDict handling around the inner packet**

Use the inner PKT for deduplication and ACK, verify before dispatch, and cache the verified embedded public key:

```rust
async fn handle_encipdict(ctx: &NetCtx, outer: &crate::ipdict::Dict, from: SocketAddr) {
    let key = from.ip().to_string();
    if !ctx.st.config().encrypt {
        ctx.st.diag(&format!("<- {from} EncIPDict 被拒绝：本机加密已关闭"));
        return;
    }
    let inner = match crypto::open_encipdict(&ctx.st.own_keypair(), outer) {
        Ok(inner) => inner,
        Err(error) => {
            ctx.st.diag(&format!("<- {from} EncIPDict 解密失败：{error}"));
            crypto_rehandshake(ctx, from, &key, "encipdict-decrypt-fail").await;
            return;
        }
    };
    let (public, capa) = match crypto::verify_ipdict(&inner) {
        Ok(Some(verified)) => verified,
        Ok(None) => {
            ctx.st.diag(&format!("<- {from} EncIPDict 缺 SIGN"));
            return;
        }
        Err(error) => {
            ctx.st.diag(&format!("<- {from} EncIPDict 验签失败：{error}"));
            return;
        }
    };
    let packet = match resolve_ipdict_packet(&inner) {
        Ok(packet) => packet,
        Err(error) => {
            ctx.st.diag(&format!("<- {from} ResolveDictMsg 失败：{error}"));
            return;
        }
    };
    ctx.st.remember_peer_key(&key, capa, &public);

    if !ctx.st.mark_seen(from.ip(), packet.pkt_no) {
        ack_encipdict(ctx, from, packet.command, packet.pkt_no).await;
        return;
    }
    if packet.command & 0xff != cmd::SENDMSG {
        ctx.st.diag(&format!("<- {from} EncIPDict 非 SENDMSG，cmd={:#x}", packet.command));
        return;
    }
    handle_sendmsg(ctx, from, &packet, &key, Some(true)).await;
    ack_encipdict(ctx, from, packet.command, packet.pkt_no).await;
}
```

`ack_encipdict` keeps the existing SENDCHECK/BROADCAST/AUTORET gate and decimal inner packet number. Use `reply_send` rather than bypassing the relay-aware reply path.

- [ ] **Step 6: Run focused network, crypto, and full Rust tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml resolve_ipdict_packet_ -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml ipdict_datagram_ -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml crypto::encipdict_tests -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml
git diff --check
```

Expected: all tests pass; output contains no compile warnings introduced by removed `Val` imports or obsolete helper functions.

- [ ] **Step 7: Commit the official receive path**

```bash
git add src-tauri/src/net.rs
git commit -m "fix(protocol): receive official IP2 EncIPDict packets"
```

---

### Task 5: Replace E10 With Official Wire and Real ACK Assertions

**Files:**
- Modify: `src-tauri/src/selftest.rs:2439-2511`
- Test: `cargo run --manifest-path src-tauri/Cargo.toml -- --selftest`

**Interfaces:**
- Consumes: signed `seal_encipdict`, `Dict::pack`, official network classifier, resolver, and ACK behavior from Tasks 1-4.
- Produces: a hermetic end-to-end regression matching Windows initial and delayed retry wire shapes.

- [ ] **Step 1: Rewrite E10 test input as full official `IP2`**

Keep the sender socket alive so it can receive the ACK. Build an ordinary visible numeric message first:

```rust
let sender = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
let enc_pkt = 665500;
let mut inner = crate::ipdict::Dict::new();
inner
    .put_int(crate::ipdict::DICT_VER, 3)
    .put_int(crate::ipdict::DICT_PKT, enc_pkt)
    .put_str(crate::ipdict::DICT_UID, "官方假对端")
    .put_str(crate::ipdict::DICT_HID, "win-host")
    .put_int(crate::ipdict::DICT_CMD, cmd::SENDMSG as i64)
    .put_int(
        crate::ipdict::DICT_FLG,
        (opt::SENDCHECKOPT | opt::UTF8OPT | opt::ENCRYPTOPT) as i64,
    )
    .put_str(crate::ipdict::DICT_BODY, "1111111");
let outer = crate::crypto::seal_encipdict(
    &ctx.st.own_keypair().public_key(),
    &kp_sender,
    &inner,
)
.unwrap();
let wire = outer.pack();
sender.send_to(&wire, target_app).await.unwrap();
```

Delete the synthetic `b"1:12345:" + pack_content + b":Z"` construction.

- [ ] **Step 2: Add real ACK capture and inner-PKT assertion**

Receive from the same socket and parse the actual response:

```rust
let mut ack_buf = [0u8; 1024];
let (ack_len, _) = tokio::time::timeout(
    Duration::from_secs(2),
    sender.recv_from(&mut ack_buf),
)
.await
.expect("RECVMSG timeout")
.expect("RECVMSG recv");
let ack = proto::parse(&ack_buf[..ack_len]).expect("classic RECVMSG");
assert_eq!(ack.command & 0xff, cmd::RECVMSG);
assert_eq!(proto::text_of(&ack).trim_end_matches('\0'), enc_pkt.to_string());
```

Record separate selftest checks for “numeric BODY on screen” and “ACK uses inner PKT”.

- [ ] **Step 3: Add the exact 64-NUL retry and duplicate assertions**

Send the same signed outer with official retry padding:

```rust
let mut retry_wire = wire.clone();
retry_wire.extend_from_slice(&[0u8; 64]);
sender.send_to(&retry_wire, target_app).await.unwrap();
```

Capture the second ACK exactly as in Step 2. Assert history contains one record with `pkt == enc_pkt`, not two, and that the second ACK still carries `enc_pkt`.

- [ ] **Step 4: Add a separate secret-message semantics check**

Create a second signed/sealed packet with a different PKT and `SECRETOPT | READCHECKOPT | ENCRYPTOPT | UTF8OPT`. Assert the emitted/history message has `secret == true` and is not silently converted into a normal visible message:

```rust
log.check(
    "E: EncIPDict 保留官方 SECRETOPT 封书语义",
    events.lock().unwrap().iter().any(|(event, value)| {
        event == "msg-in"
            && value["msg"]["pkt"].as_u64() == Some(secret_pkt as u64)
            && value["msg"]["secret"].as_bool() == Some(true)
    }),
);
```

- [ ] **Step 5: Run E10 through the full selftest and verify GREEN**

Run:

```bash
cargo run --manifest-path src-tauri/Cargo.toml -- --selftest
```

Expected: exit code 0; E10 reports PASS for full `IP2`, numeric BODY, exact 64-NUL retry, duplicate deduplication, two ACKs with inner PKT, signature validation, and secret semantics. Any FAIL line means this task is not complete.

- [ ] **Step 6: Run unit tests after selftest state changes**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
git diff --check
```

Expected: all Rust tests pass.

- [ ] **Step 7: Commit E10**

```bash
git add src-tauri/src/selftest.rs
git commit -m "test(protocol): replay official EncIPDict retry flow"
```

---

### Task 6: Correct Documentation and Run Final Verification

**Files:**
- Modify: `README.md:199-205`
- Verify: all files changed by Tasks 1-5

**Interfaces:**
- Consumes: completed implementation and tests.
- Produces: accurate compatibility documentation and fresh final evidence.

- [ ] **Step 1: Correct the README wire-format statement**

Replace the incorrect `1:<包号>:` wording with:

```markdown
- 官方 5.8.x 的 v5 新格式密文消息（EncIPDict，完整
  `IP2:<内容长度>:EF/EI/EK/EB...:Z` 字典报文）已支持：首次发送与 delayed
  重试追加的 64 字节 NUL 填充均按官方源码语义解析、验签并确认送达。
```

Do not claim that the unpublished 5.8.6 source was ported line by line.

- [ ] **Step 2: Run fresh full automated verification**

Run as one fail-fast sequence:

```bash
set -o pipefail
cargo test --manifest-path src-tauri/Cargo.toml
pnpm test
pnpm build
cargo run --manifest-path src-tauri/Cargo.toml -- --selftest
git diff --check
```

Expected:
- Rust unit/doc tests: 0 failed;
- Node tests: 0 failed;
- Vite production build: exit 0;
- selftest: every scenario PASS and exit 0;
- `git diff --check`: no output and exit 0.

Do not use full-tree `cargo fmt --check` as the acceptance gate because the repository already has unrelated baseline formatting drift. Do not run `cargo fmt` over the entire tree.

- [ ] **Step 3: Inspect scope before the final commit**

Run:

```bash
git status --short
git diff --stat HEAD
git diff -- README.md src-tauri/src/ipdict.rs src-tauri/src/crypto.rs \
  src-tauri/src/net.rs src-tauri/src/selftest.rs
```

Expected: only the planned protocol/test/documentation files are modified; no capture file, key material, application data, `dist`, or `/tmp` artifact is tracked.

- [ ] **Step 4: Commit documentation**

```bash
git add README.md
git commit -m "docs(protocol): document official EncIPDict wire format"
```

- [ ] **Step 5: Restart the dev client from the verified binary**

Stop the existing `target/debug/open-ipmsg` dev instance through its normal dev runner, then launch the freshly built client with diagnostics enabled:

```bash
pnpm tauri dev -- -- --log
```

If the existing Tauri dev runner owns the process, restart that runner instead of killing unrelated processes. Verify `ss -lunp` shows exactly one `open-ipmsg` listener on UDP 2425.

- [ ] **Step 6: Perform Windows 5.8.6 acceptance tests**

From Windows IP Messenger 5.8.6 at `10.200.230.3`, send to `10.200.230.254`:

```text
official-ip2-probe
1111111
abcdef
```

Also send one sealed message and allow one unacknowledged test message to pass the delayed retry threshold so the sender emits the 64-NUL variant.

Verify:
- every ordinary message appears with byte-for-byte identical text;
- the sealed message appears as sealed, not silently downgraded;
- delayed retry does not duplicate history;
- Windows removes each acknowledged item from its delayed queue;
- `diag.log` contains IP2 parse, EncIPDict decrypt, signature success, inner PKT persistence, and same-PKT RECVMSG;
- `diag.log` does not contain `user="EF" host="6" cmd=0x0007a124` for the new packets.

- [ ] **Step 7: Report verified and unverified results separately**

The final report must list exact automated command results and the observed Windows cases. If Windows testing is unavailable or any case still fails, say “未验证” or “失败” for that case; do not infer success from selftest alone.

---

### Task 7: Auto-Display Unpassworded Sealed Messages

**Files:**
- Modify: `src-tauri/src/net.rs:1623-1653`
- Modify: `src-tauri/src/selftest.rs:2180-2252,2528-2568`
- Test: existing Rust unit/integration tests and full `--selftest`

**Interfaces:**
- Consumes: existing `secret`, `locked`, `unlocked`, `need_read`, `mark_read_and_receipt`, and EncIPDict receive semantics.
- Produces: unpassworded `SECRETOPT` records with `secret=true`, `locked=false`, `unlocked=true`; password-protected records remain locked; no READMSG is sent until the chat becomes visible and calls the existing mark-read path.

- [ ] **Step 1: Write failing receive-state tests**

Add or update tests to assert:

```rust
assert_eq!(record["secret"].as_bool(), Some(true));
assert_eq!(record["locked"].as_bool(), Some(false));
assert_eq!(record["unlocked"].as_bool(), Some(true));
assert_eq!(record["read"].as_bool(), Some(false));
```

Before calling `mark_read_and_receipt`, assert the fake peer has received no READMSG. Then call the existing mark-read path and assert exactly one READMSG for that packet. Keep the password-message test asserting `locked=true` and `unlocked=false` until the correct password is supplied.

- [ ] **Step 2: Run focused tests and verify RED**

Run the narrow test(s) that exercise classic inbound `SECRETOPT` and EncIPDict inbound `SECRETOPT`.

Expected: FAIL because the current record initializes `unlocked` only from `prev_unlocked`, so a new unpassworded sealed message remains hidden.

- [ ] **Step 3: Implement automatic local unseal without eager read receipt**

Compute initial state once in `handle_sendmsg`:

```rust
let auto_unlocked = secret && !locked;
let unlocked = prev_unlocked || auto_unlocked;
```

Write the record as:

```rust
"secret": secret,
"locked": locked && !unlocked,
"unlocked": unlocked,
```

Do not clear `SECRETOPT`, do not set `read=true`, and do not call `mark_read_and_receipt` from the network receive handler. The existing frontend visibility/open-chat path remains the only trigger for READMSG.

- [ ] **Step 4: Update E4 and E10 assertions**

E4 must assert auto-unlocked state, no receipt before explicit `mark_read_and_receipt`, one receipt after it, and no duplicate receipt. E10's signed/sealed EncIPDict message must assert both `secret=true` and `unlocked=true`. Password E5 must remain locked until successful password verification.

- [ ] **Step 5: Run complete verification**

```bash
cargo test --manifest-path src-tauri/Cargo.toml
cargo run --manifest-path src-tauri/Cargo.toml -- --selftest
pnpm test
git diff --check
```

Expected: all commands exit 0 with no new warnings or FAIL lines.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/net.rs src-tauri/src/selftest.rs
git commit -m "fix(chat): auto-display unpassworded sealed messages"
```
