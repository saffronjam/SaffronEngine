export type EnvironmentSection = "sky" | "time" | "weather" | "fog";
export type DetailLevel = "essential" | "all";

const VIEW_STATE_KEY = "saffron.environment-panel.v1";

export interface EnvironmentViewState {
  section: EnvironmentSection;
  detail: DetailLevel;
  expanded: Record<string, boolean>;
}

const DEFAULT_VIEW_STATE: EnvironmentViewState = {
  section: "sky",
  detail: "essential",
  expanded: {
    background: true,
    lighting: true,
    atmosphere: false,
    clock: true,
    location: false,
    automation: false,
    clouds: true,
    wind: false,
    fogAppearance: true,
    fogLighting: false,
    groundHaze: false,
    aerialPerspective: false,
  },
};

export function loadViewState(): EnvironmentViewState {
  try {
    const saved = JSON.parse(
      localStorage.getItem(VIEW_STATE_KEY) ?? "null",
    ) as Partial<EnvironmentViewState> | null;
    return {
      section: saved?.section ?? DEFAULT_VIEW_STATE.section,
      detail: saved?.detail ?? DEFAULT_VIEW_STATE.detail,
      expanded: { ...DEFAULT_VIEW_STATE.expanded, ...saved?.expanded },
    };
  } catch {
    return DEFAULT_VIEW_STATE;
  }
}

export function saveViewState(state: EnvironmentViewState): void {
  localStorage.setItem(VIEW_STATE_KEY, JSON.stringify(state));
}
