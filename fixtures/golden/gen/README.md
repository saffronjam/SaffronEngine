# Golden-fixture generator

`gen_golden.cpp` emits the byte-exact reference fixtures in the parent directory. Its writer
logic is independently mirrored from each format owner (see `../PROVENANCE.md` for the
per-fixture source map and the C++ reference toolchain/commit).

It is a **standalone** program: the on-disk formats are pure `#[repr(C)]`/JSON data, so it
links nothing but the standard library and the engine's vendored `nlohmann/json` (v3.12.0),
not Vulkan/Jolt/SDL. This makes the reference disk bytes and current GPU ABI bytes reproducible
without a full engine build.

```sh
toolbox run -c saffron-build bash -lc '
  cd <repo>
  INC=build/debug/_deps/nlohmann_json-src/single_include
  clang++ -std=c++26 -I"$INC" -o /tmp/gen_golden fixtures/golden/gen/gen_golden.cpp
  /tmp/gen_golden fixtures/golden
'
```

The Rust snapshot tests reproduce the same fixture inputs (the unit cube, the clip, the
populated material, the known-valued std430 instances) field-for-field and assert the bytes
match. Keep this generator and those tests in lockstep: a change to a fixture input here is a
change there.
