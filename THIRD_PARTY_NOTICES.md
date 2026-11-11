# Avisos de terceros

## AMD FidelityFX Super Resolution 1.0 (EASU y RCAS)

`crates/renuevo-core/src/upscale/easu.rs` y `crates/renuevo-core/src/upscale/rcas.rs` son ports a
Rust (CPU, planar f32) de los algoritmos EASU y RCAS de `ffx_fsr1.h`.

```
Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## Algoritmos publicados en los que se basa el código

- Non-Local Means: A. Buades, B. Coll, J.-M. Morel, «A non-local algorithm for image denoising», CVPR 2005.
  Aceleración por sumas: J. Darbon et al., «Fast nonlocal filtering applied to electron cryomicroscopy», ISBI 2008.
- Filtro guiado: K. He, J. Sun, X. Tang, «Guided Image Filtering», ECCV 2010.
- Filtro de bloques estilo H.264: ITU-T H.264, sección 8.7 (deblocking).
