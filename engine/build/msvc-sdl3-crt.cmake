# Sektor7 engine — SDL3 MSVC runtime-library alignment
# (Stage 1 corrective pass, review finding 2 — LNK4098).
#
# Cause: the vendored SDL build is configured by cmake-rs with
# CMAKE_BUILD_TYPE=Debug for cargo dev builds, and MSVC's Debug
# configuration selects the debug CRT (/MDd — MSVCRTD). Rust links the
# release dynamic CRT even in debug builds, so the final binary mixes
# defaultlib entries and the linker reports:
#   LINK : warning LNK4098: defaultlib 'MSVCRTD' conflicts with use of other libs
#
# Solution: force the release dynamic CRT (/MD, MultiThreadedDLL) for the
# SDL build in EVERY configuration, aligning SDL's objects with Rust's.
# The underlying runtime mismatch is corrected — no /NODEFAULTLIB and no
# warning suppression is involved. SDL still builds with debug information
# and its own DEBUG define; only the CRT selection changes.
#
# Delivery: engine/.cargo/config.toml sets the target-suffixed
# CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_msvc environment variable
# (resolved relative to the workspace root, so it is portable across
# machines). cmake-rs reads it before configuring the SDL build for
# x86_64-pc-windows-msvc targets; non-Windows targets never look it up by
# that name, so this file is inert on Linux.
#
# Verification: the Windows CI log contains zero LNK4098 warnings after
# this change (four occurrences before).
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL" CACHE STRING
    "SDL3 builds against the release dynamic CRT to match Rust" FORCE)
