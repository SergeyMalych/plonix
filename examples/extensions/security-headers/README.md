# security-headers

A small Plonix extension, and a template for writing your own. For every in-scope HTML page it is given, it notes the security headers that are missing (`Content-Security-Policy`, `X-Content-Type-Options`, `Strict-Transport-Security` on HTTPS, `X-Frame-Options` when there is no CSP) and the cookies set without `Secure` or `HttpOnly`. Then it proposes one finding per host, which stays open until you confirm it.

It asks for `read-traffic`, `passive-analysis` and `propose-findings`, and nothing else. See [docs/extensions.md](../../../docs/extensions.md) for the host API and the sandbox it runs in.

## Try it

```sh
plonix extensions add examples/extensions/security-headers --yes
plonix extensions run security-headers      # over the traffic captured so far
plonix findings
```

## Build

The committed `security_headers.wasm` is built from `src/lib.rs`:

```sh
rustup target add wasm32-unknown-unknown
cd examples/extensions/security-headers
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/security_headers.wasm .
plonix extensions pack . -o ../../../store/extensions/security-headers.plonixext
```

`plonix extensions pack` prints the package's SHA-256, which is what a Market index lists for it. Built with Rust 1.97; a different compiler may produce different bytes, and so a different checksum.
