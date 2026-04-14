# posthog-rs

## 0.5.1 — 2026-04-14

### Patch changes

- [543ca47](https://github.com/posthog/posthog-rs/commit/543ca47c0d1c1365ca13a1882ea7089851bef19a) Update reqwest from 0.11.3 to 0.13.2 to replace the unmaintained feature "rustls-tls" with "rustls" (RUSTSEC-2025-0134) — Thanks @marandaneto!

## 0.5.0 — 2026-03-27

### Minor changes

- [842bb73](https://github.com/posthog/posthog-rs/commit/842bb73c17c9fce63df04d01d93a0c78b91e1a63) Add `local_evaluation_only` option to prevent remote API fallback when local evaluation is inconclusive — Thanks @dmarticus!

## 0.4.7 — 2026-03-20

Note: 0.4.4 - 0.4.6 were never released due to a bug in our release process. Commit signing is hard!

### Patch changes

- [6a04431](https://github.com/posthog/posthog-rs/commit/6a04431ecd66d85360500e15dbc28a19bb349d21) Sign commits during release process — Thanks @marandaneto!

## 0.4.3 — 2026-03-05

### Patch changes

- [b1b109d](https://github.com/posthog/posthog-rs/commit/b1b109dcdc52d9a0fd72268a44b3e367e593d8b1) Add semver comparison operators for local feature flag evaluation — Thanks @dmarticus!

## 0.4.2 — 2026-02-23

### Patch changes

- [d94ecbb](https://github.com/posthog/posthog-rs/commit/d94ecbb4e6960e775f18f7b81664c18fa35ddc12) Historical batch capture support — Thanks @z0br0wn!
- [6af1786](https://github.com/posthog/posthog-rs/commit/6af178641092740ac1cca24f08d1a1fc760f2cb1) Add Capture API response handling — Thanks @z0br0wn!
- [3fdab70](https://github.com/posthog/posthog-rs/commit/3fdab70a3a6b2310ed1f7772cf742b184104bedf) Generate (and allow overrides of) event UUID, allow for properties pass through. — Thanks @z0br0wn!

## 0.4.1 — 2026-02-18

### Patch changes

- [c15b195](https://github.com/posthog/posthog-rs/commit/c15b195728be26de67a66d64d04ada7e3b729351) Migrate release process to Sampo for automated versioning, changelogs, and publishing. — Thanks @rafaeelaudibert!

## 0.4.0 - 2026-02-04

- Add feature flags with local evaluation support (#36)
- Add group support and anonymous event support (#22)
- Add global disable function (#20)
- Add global client functions (#19)
- Add timestamp setter (#30)

## 0.2.0

- Add generic properties (#1)
- Derive Debug, PartialEq, and Eq on Event
