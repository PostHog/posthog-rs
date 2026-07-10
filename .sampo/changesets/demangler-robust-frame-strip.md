---
cargo/posthog-rs: patch
---

fix: strip SDK capture frames under newer demangler renderings (`<Type>::method::<T>`), which previously survived at the crash-site end of the stack
