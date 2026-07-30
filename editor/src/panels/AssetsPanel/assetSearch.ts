import type { ChipConfig } from "../../components/anima/chipSearch";
import type { AssetSortMode } from "../../state/store";

const ASSET_TYPE_VALUES = [
  "mesh",
  "texture",
  "material",
  "animation",
  "model",
  "lut",
  "environment",
  "plant",
  "biome",
  "vegetation-map",
  "other",
] as const;

const sentenceCase = (value: string): string => value.charAt(0).toUpperCase() + value.slice(1);

/// The Assets search bar's single chip: `type:<kind>` narrows the grid to one asset kind. The
/// lowercase value matches `asset.type`; suggestions and the committed chip show it sentence-cased.
export const ASSET_SEARCH_CHIPS: ChipConfig[] = [
  {
    keyword: "type",
    label: "Type",
    options: (input) => {
      const query = input.toLowerCase();
      return ASSET_TYPE_VALUES.filter((value) => value.includes(query)).map((value) => ({
        value,
        label: sentenceCase(value),
      }));
    },
    resolveLabel: sentenceCase,
  },
];

export const ASSET_SORT_OPTIONS: { value: AssetSortMode; label: string }[] = [
  { value: "name-asc", label: "Name (A–Z)" },
  { value: "name-desc", label: "Name (Z–A)" },
  { value: "created-desc", label: "Newest first" },
  { value: "created-asc", label: "Oldest first" },
];
