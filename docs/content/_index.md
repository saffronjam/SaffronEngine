+++
title = 'Saffron Anima'
description = 'How the Saffron Anima renderer works, concept by concept, with links into the code that implements it.'
+++

# Saffron Anima

Saffron Anima is a from-scratch **Vulkan** renderer and game engine written in **Rust**,
with a CEF/React editor. These docs explain how it renders, concept by concept, and
link each explanation to the code behind it.

The code favours small data structs, free functions, and errors as values over deep
inheritance. That style runs through the whole engine, so read [the conventions](explanations/core-and-conventions/)
before the deeper graphics pages.

## Where to start

- **Overview:** the [Overview](overview/) is a one-screen tour from window to pixel.
- **Subsystems:** [Explanations](explanations/) covers each part in its own page — render graph, lighting, shadows, post-processing, the editor.
- **Tasks:** [How-to](how-to/) has recipes (build and run, import a model, drive the `sa` CLI).
- **Signatures and commands:** [Reference](reference/) is the flat catalog.
- **Guided builds:** [Tutorials](tutorials/) walk through complete tasks end to end.

## How these docs are organised

The four sections follow the [Diátaxis](https://diataxis.fr/) framework: explanations
cover how and why something works, how-tos are recipes, reference is lookup, and
tutorials are guided builds.
