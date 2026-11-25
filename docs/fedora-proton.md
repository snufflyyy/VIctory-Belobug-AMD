# Fedora with Proton Experimental

VIctory is a Windows build. On Fedora, Proton translates its Windows calls and vkd3d-proton translates D3D12 to Vulkan. This path is experimental. Startup or visible menus do not establish that the 3D world works.

## Build and run

1. Install Proton Experimental from Steam. Install Fedora's `bubblewrap` and `mangohud` packages.
2. Install Rust with the repository's Windows target, LLVM tools and `cargo-xwin`.
3. From the checkout, build the diagnostic executable:

   ```sh
   cargo xwin build --locked --target x86_64-pc-windows-msvc \
     --profile diag -p victory --features diagnostics
   ```

4. From your desktop session, load your copy without executing it. Replace `/path/to/game_scarlett_master.exe` with the path to your own file:

   ```sh
   sh scripts/fedora-proton.sh --check \
     target/x86_64-pc-windows-msvc/diag/victory.exe /path/to/game_scarlett_master.exe
   ```

5. Remove `--check` to start the game. Close the window or press Ctrl+C in the launch terminal to stop. With the window focused, press Enter for A and use WASD and IJKL for movement and camera.

The launcher keeps saves, logs from crashes, and caches under `${XDG_STATE_HOME:-$HOME/.local/state}/victory-proton`. Set `VICTORY_PROTON_STATE` to use a different folder. Set `STEAM_ROOT` or `PROTON_DIR` for a nonstandard Steam or Proton install. It mounts game files read-only and uses a private Proton prefix. Leave SELinux enforcing.

## Test window and overlay

The launcher caps the game window at 1280×720. This scales the displayed image but does not force the game to render at 720p. MangoHud is on by default. Set `MANGOHUD=0` to hide it, or set `MANGOHUD_CONFIG` to change the displayed metrics.

## Performance checks

Run each comparison from the same save and camera position. Keep the window, graphics settings and driver fixed. Repeat with a warm cache. Check median frame time, GPU time and dropped draws together:

```sh
python3 scripts/proton-frame-summary.py first.log second.log
```

Use separate logs for different runs. The script summarizes the periodic frame logs. Its draw-count filter does not guarantee that two scenes match.

`VICTORY_WINESERVER` selects an optional local server built for the exact installed Proton version. The launcher mounts it inside the sandbox only. Leave it unset to use stock Proton.

## Radeon notes

- The launcher sets `VICTORY_COPY_MEMORY=1`. On Fedora/RADV, the in-place GPU path produced a black window. The copy path restored the menus. See [Belobug issue #19](https://github.com/Belobug/VIctory-Belobug-AMD/issues/19).
- If Wine's shader compiler rejects the internal indirect-argument shaders, set `VICTORY_NATIVE_FXC` to a trusted 64-bit Microsoft `d3dcompiler_47.dll`. The launcher does not download or redistribute the DLL. See the [RDNA 3 Proton report](https://github.com/fxceless/victory-fork/issues/184).
- If version 1.0.2.1 stops at `+0x23cf683` while loading the world, `VICTORY_NO_WATCHDOG=1` gives the load more time. It does not fix a stall. Stop the test if progress stops.
- Do not enable experimental asynchronous GPU switches by default. A GPU hang can affect the desktop, not only this process.
