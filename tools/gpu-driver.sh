# Point the Vulkan loader at this platform's driver. Source it; do not execute it.
#
# The Linux ICD path is exact and load-bearing: the NVIDIA manifest lives under the host mount, not
# the toolbox's own /usr, and its filename carries the `.x86_64` suffix. A wrong path resolves to
# Mesa llvmpipe, which reads as "no GPU here" while the card is present.
#
# Every branch ends successfully so that sourcing this under `set -e` cannot abort the caller on a
# machine where the driver is simply absent.

if [ "$(uname)" = "Darwin" ]; then
  for icd in /opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json /usr/local/etc/vulkan/icd.d/MoltenVK_icd.json; do
    if [ -f "$icd" ]; then
      export VK_ICD_FILENAMES="$icd"
      break
    fi
  done
  if [ -z "${VK_LAYER_PATH:-}" ]; then
    for prefix in /opt/homebrew /usr/local; do
      layer_manifest="$prefix/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d"
      layer_library="$prefix/opt/vulkan-validationlayers/lib"
      if [ -d "$layer_manifest" ] && [ -d "$layer_library" ]; then
        export VK_LAYER_PATH="$layer_manifest"
        export DYLD_FALLBACK_LIBRARY_PATH="$layer_library${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
        break
      fi
    done
  fi
else
  for icd in /run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json /usr/share/vulkan/icd.d/nvidia_icd.x86_64.json; do
    if [ -f "$icd" ]; then
      export VK_ADD_DRIVER_FILES="$icd"
      break
    fi
  done
fi
