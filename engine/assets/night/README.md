# Night-sky source assets

`bsc5.bin` is the compact runtime form of the Yale Bright Star Catalog, Fifth Revised Edition. It
contains J2000 directions, Pogson-relative luminance, and CIE-blackbody linear-sRGB color for every
catalog row with a valid position and visual magnitude.

Source: [Yale Bright Star Catalog, version 5](http://tdc-www.harvard.edu/catalogs/bsc5.html),
distributed by the Smithsonian Astrophysical Observatory Telescope Data Center. Regenerate from
the decompressed `ybsc5` fixed-width file with:

```sh
cd engine
cargo run -p xtask -- bake-stars /path/to/ybsc5
```
