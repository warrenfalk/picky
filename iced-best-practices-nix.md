# Iced Best Practices on Nix

This document captures the Nix-specific lessons from getting an Iced app running correctly with the GPU stack.

## Core Rule

Do not build a custom mixed graphics stack in your flake shell.

Iced apps using `wgpu` depend on a coherent runtime stack:

- Vulkan loader
- EGL / GL loader
- Mesa driver ICDs
- Wayland client library
- input libraries like `libxkbcommon`

If these come from mismatched versions, `wgpu` may fail before it can enumerate adapters.

## What Went Wrong

The initial flake exported a broad `LD_LIBRARY_PATH` containing:

- `wayland`
- `libGL`
- `vulkan-loader`
- X11 and related libs

That looked reasonable, but it caused a real ABI mismatch:

- the active Vulkan ICD came from `/run/opengl-driver`
- the Mesa Vulkan driver expected a newer `libwayland-client.so.0`
- the shell forced an older flake-provided Wayland client first
- Vulkan driver loading failed on `wl_display_dispatch_queue_timeout`
- `wgpu` then reported no compatible adapter

This was not a GPU support problem. It was a loader/runtime mismatch.

## Best Practices

### 1. Keep `LD_LIBRARY_PATH` minimal

Do not stuff general graphics libraries into a broad shell-wide `LD_LIBRARY_PATH`.

Prefer a minimal runtime path with only the loader pieces you actually need:

- `libxkbcommon`
- `libglvnd`
- `vulkan-loader`

Do not blindly add:

- `wayland`
- `libGL`
- large GTK/X11/Wayland closures

unless you are sure they are compatible with the actual driver stack in use.

### 2. Do not assume `pkgs.wayland` matches the active driver stack

The Wayland client library used by the compositor and by your flake may not match the Wayland client library required by the active Mesa Vulkan ICD.

If you need a `libwayland-client.so.0` in the runtime path, derive it from the active Vulkan ICD instead of assuming your flake's Wayland package is correct.

In practice, the robust approach was:

1. read the active ICD JSON from `/run/opengl-driver/share/vulkan/icd.d/*.json`
2. extract its `library_path`
3. run `ldd` on that ICD with `LD_LIBRARY_PATH` cleared
4. use the `libwayland-client.so.0` path it resolves

That ensures the Wayland client comes from the same closure the driver expects.

### 3. Treat graphics libraries as a coherent stack

These are not ordinary independent shared libraries.

The following are tightly coupled:

- Vulkan ICDs
- Vulkan loader
- EGL / GL loaders
- Mesa
- Wayland client libs
- DRM/render node access

Mixing stack pieces from different closures can produce failures that look like:

- `GraphicsAdapterNotFound`
- `Found no drivers`
- `ERROR_INCOMPATIBLE_DRIVER`
- `NoWaylandLib`

without any obvious sign that the GPU itself is fine.

### 4. Prefer narrow runtime wrapping over broad shell mutation

If you need to wrap a packaged binary, use a narrow `wrapProgram` configuration.

Good:

- set only the minimal loader path
- derive any driver-coupled libraries from the active driver stack

Bad:

- prepend a giant list of GUI and graphics libraries into `LD_LIBRARY_PATH`

### 5. Verify the stack directly

Before assuming Iced or `wgpu` is broken, check the runtime stack explicitly.

Useful tools:

- `vulkaninfo --summary`
- `eglinfo -B`
- `glxinfo -B`
- a tiny local `wgpu` probe

These help separate:

- Vulkan loader failures
- EGL / GL failures
- device permission failures
- `wgpu` adapter-selection failures

### 6. Separate loader issues from permission issues

Two independent problems can exist at the same time.

In our investigation:

- first problem: broken Vulkan ICD loading due to Wayland ABI mismatch
- second problem in the sandbox: `/dev/dri/renderD128` permission denial, which forced software fallback

Fixing the loader problem did not remove the sandbox permission issue, but it did prove the original root cause.

## Recommended Flake Pattern

For an Iced app dev shell:

- include debugging tools like `vulkan-tools` and `mesa-demos`
- avoid broad graphics-library injection
- set a minimal `LD_LIBRARY_PATH`
- dynamically append the Wayland client directory resolved from the active Vulkan ICD

For the packaged app:

- use the same minimal runtime strategy
- avoid wrapping with an arbitrary flake-provided Wayland library unless you know it matches the active driver closure

## Practical Checklist

1. Add `vulkan-tools` and `mesa-demos` to the dev shell.
2. Verify `vulkaninfo`, `eglinfo`, and `glxinfo`.
3. Test `ICED_BACKEND=wgpu`.
4. If `wgpu` finds no adapter, inspect loader/runtime mismatches before blaming the GPU.
5. Keep `LD_LIBRARY_PATH` minimal.
6. Do not mix flake-provided Wayland with system Mesa unless you know they are compatible.
7. Distinguish adapter-detection failure from device-permission failure.

## One-Sentence Summary

When packaging an Iced app on Nix, do not assemble your own mixed graphics runtime by convenience; let `wgpu` see a coherent driver stack, and only add the smallest loader/runtime pieces you actually need.
