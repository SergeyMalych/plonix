# js-endpoints

A small Plonix analyzer extension, and a template for writing a passive one. For every in-scope JavaScript response it is given, it reads the string literals in the code and pulls out the ones that look like API paths and URLs the app talks to (`/api/...`, `https://host/...`), then notes them on that response. Those are endpoints a page reaches at run time but that may not show up in captured traffic until something triggers them, so they are a map of where to look next.

It asks for `read-traffic` and `passive-analysis`, and nothing else: it reads responses it is given and sends nothing. See [docs/extensions.md](../../../docs/extensions.md) for the host API and the sandbox it runs in.

## Try it

```sh
plonix extensions add examples/extensions/js-endpoints --yes
plonix extensions run js-endpoints      # over the traffic captured so far
# then open a script in Traffic: the endpoints show in the Lens
```

## Build

The committed `js_endpoints.wasm` is built from `src/lib.rs`:

```sh
rustup target add wasm32-unknown-unknown
cd examples/extensions/js-endpoints
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/js_endpoints.wasm .
plonix extensions pack . -o ../../../store/extensions/js-endpoints.plonixext
```

`plonix extensions pack` prints the package's SHA-256, which is what a Market index lists for it. A different compiler may produce different bytes, and so a different checksum.
