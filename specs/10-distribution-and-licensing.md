# Specification 10 — Distribution & Licensing
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §10 open decision #3 (licensing/distribution), hardened-product posture · **Status:** DRAFT for review
**Context:** LedgerOne ships to multiple SME clients as a product, not an internal tool. This spec fixes the distribution seams now so nothing built in Steps 1–4 has to be reworked for them.

---

## 1. Installer & Code Signing (Windows-first)

- **Bundle:** Tauri v2 **NSIS installer** (`.exe`) — per-user install (no admin prompt, matches SME reality of non-admin laptops), desktop + start-menu shortcuts, proper uninstall. MSI deferred until an enterprise client demands GPO deployment.
- **Targets:** **x64 is the primary client target** (typical Nigerian SME laptops); ARM64 secondary (dev machine + emerging Snapdragon laptops). CI builds both; the dev box cross-compiles x64 via the already-installed `Hostarm64\x64` MSVC tools + `rustup target add x86_64-pc-windows-msvc`.
- **Authenticode signing:** unsigned builds trip SmartScreen — a client-facing product cannot ship unsigned. v1: **Azure Trusted Signing** (subscription-cheap, HSM-backed, integrates with CI); fallback: OV cert from a conventional CA. EV only if SmartScreen reputation proves too slow to accumulate. **Entity CONFIRMED 2026-07-04: Bolton Advisory Group** — CAC registration details to follow; placeholder in build config until provided (no blocker). *(Decision #1 — resolved)*

## 2. Update Mechanism

- **`tauri-plugin-updater`**, static JSON manifest + signed artifacts on a dumb host (GitHub Releases or Cloudflare R2 — no server code). Offline-first discipline applies: update checks are background, **never block work**, and a client can decline indefinitely; unsupported-version nagging is a banner (Spec 07 §2.3 slot), not a lock.
- **Updater signing keys are generated NOW, before any client install exists** (`tauri signer generate` → minisign keypair): public key embedded in `tauri.conf.json` from the first shipped build; **private key + password live offline in the practice's password manager, never in the repo, never on the build machine's disk unencrypted**. Key loss = clients must manually reinstall forever; treat the private key like the company seal. *(Decision #2)*
- Staged rollout (percentage/cohort) deferred; single channel `stable` in v1.

## 3. Licensing / Activation (v1: honest and minimal)

- **v1 model:** a license key string entered at company creation, stored in `companies.license_key` (schema migration 0002 — field exists from the first shipped build so no future migration touches client data for this). **Not validated online in v1** — offline-first, and the advisory-bundled distribution model (planning doc open decision #3) means the relationship, not the key, is the enforcement. The key's v1 job is *identification* (which client, which engagement) on backups, exports, and support bundles.
- **Key format (fixed now so v2 validation is backward-compatible):** `LO-` + base32 payload + checksum, payload reserved for {client id, seat count, expiry, edition} — v2 signs this payload with an Ed25519 key verified offline by the app. No phone-home requirement ever for core bookkeeping (product principle 1).
- Distribution model default: **advisory-bundled** (key issued per engagement); standalone sale uses the same key mechanics later. *(Decision #3)*

## 4. Per-Client Data Isolation

- **One machine, one database file** (multi-company within it is for the *advisor* persona managing clients on the advisor's own machine — a client machine holds only that client's companies). The installer/app never syncs data anywhere by default; Sheets push (Spec 09) and Drive backup (Phase 2) are opt-in per company.
- **Isolation boundaries, stated plainly:** (a) between client machines — physical, nothing shared; (b) on one machine — the OS user profile (`%APPDATA%\LedgerOne`); the app documents that shared-Windows-login laptops share visibility, and the Advisor PIN gates *capabilities*, not *data visibility*; (c) within a database — every Tauri command is company-scoped server-side (command layer resolves `company_id` from the active session, never trusts the frontend to scope queries). *(Decision #4)*
- **Client offboarding:** Spec 06 export-everything + a Spec 08 backup file constitute the complete handover; no data is retained by the app vendor because the app vendor never had it.

## 5. ⚠️ Regulatory-adjacent flags (standing rule — for your explicit read)

Nigeria Data Protection Act 2023 / NDPR: client books contain **personal data** (customer/supplier names, phones, addresses, TINs). Flagging the postures this spec implies:

1. **Local-first is the privacy posture** — data lives on the client's machine; the client is the data controller; LedgerOne (the software) processes nothing remotely, v1 has **zero telemetry**, and this spec keeps it that way. If telemetry/crash reporting is ever added, it needs an NDPA-compliant notice and opt-in at that time — flagged as a future gate, not a current task.
2. **Bolton Advisory as processor:** when the advisory practice holds copies of client books (advisor machine multi-company, synced Sheets, backup files received for support), the *practice* — not the app — is acting as a data processor and should have a data-processing clause in the engagement letter. **App-side obligation:** make what leaves the machine visible and consensual (already specced: Spec 09 §4 consent surface; Spec 08 backup destination is user-chosen).
3. **Cross-border transfer:** Sheets push and (Phase 2) Drive backup move personal data to Google infrastructure outside Nigeria. NDPA permits transfer with consent/adequacy mechanisms; the Spec 09 consent screen should say this in plain language ("this sends your customer list and figures to Google's servers"). One sentence added to that screen's copy — flagged here, applied in Spec 09 implementation. *(Decision #5)*

None of these change schema or engine behavior; they change wording on two consent screens and one engagement-letter template (the latter being practice-side, outside the app).

## 5b. First-Run EULA / Disclaimer Gate (added per review 2026-07-04)

A **click-through EULA/disclaimer screen precedes the setup wizard** on first run — the wizard does not open until accepted. Content (full text: [docs/legal/EULA.md](../docs/legal/EULA.md), maintained as its own document, not buried here): no warranty of accuracy for calculated figures; the user's responsibility to verify before any regulatory filing; no advisory or professional relationship created by software use alone; limitation of liability. Mechanics: acceptance records an append-only `audit_log` row (`eula.accepted`, version + timestamp — the audit log's immutability is exactly what makes it the right evidence store) and the accepted version is passed into company creation; a future EULA version re-gates on next launch. ⚠️ **The drafted text is NOT lawyer-reviewed — do not distribute outside internal/EdenOceans use until it is** (flagged in PROGRESS.md). *(Decision #6)*

## 6. Deltas

- Migration **0002**: `ALTER TABLE companies ADD COLUMN license_key TEXT;` — `seed::create_company` accepts and stores it. No validation logic in v1.
- No other schema/engine changes. Updater plugin and NSIS config land when the shell ships its first installable build.

## 7. Decisions needing your sign-off

1. **NSIS per-user installer; x64 primary target; Azure Trusted Signing under a named legal entity** (which entity — your call). (§1)
2. **Updater keypair generated before first client install; private key custody = practice password manager, offline.** (§2)
3. **v1 license key = stored identifier, offline, unvalidated; format fixed now for v2 signed payloads; advisory-bundled default.** (§3)
4. **Isolation model as stated** — OS profile is the on-machine boundary; command-layer company scoping; no vendor-side data, ever. (§4)
5. ✅ **NDPA flags (§5) — CONFIRMED by reviewer** (recorded 2026-07-04 at reviewer's direction): all three postures stand — local-first/zero-telemetry as the privacy posture; processor clause lives in the engagement letter, not the app; cross-border plain-language sentence goes on the Spec 09 consent screen at implementation. (§5)
6. **First-run EULA gate as specified** — click-through before wizard, audit-log acceptance record, re-gate on version change; text pending legal review. (§5b)

---

*End of Spec 10.*
