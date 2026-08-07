/// Image formation after scene rendering: tone mapping and exposure, color grading, and screen-space
/// effects, one tab each. Like Render and Environment these settings persist with the project and
/// are scene-tab undoable.
///
/// Values are read through a shallow-selected subset of `renderStats` so the body re-renders only
/// when a bloom or grade field changes, never on the stats poll. Writes fold the new value (and the
/// echoed result) in optimistically; a scrub records one undo entry and gates the poll via
/// `dragActive`.
import { NumberDrag } from "../../components/NumberDrag";
import { ColorField } from "../../components/ColorField";
import { SliderField } from "../../components/SliderField";
import { ControlRow, FieldRow, SectionBreak } from "../../components/PanelRows";
import { ToneCurve } from "../../components/ToneCurve";
import { errorText, notify, notifyError } from "../../lib/flash";
import { client } from "../../control/client";
import { ASSET_DND_MIME, assetIdsFromPayload, readAssetPayload } from "../../components/AssetTile";
import type { GradeRangeDto } from "../../protocol";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { CdlWheels } from "./CdlWheels";
import { MIXER_OUTPUTS, TONEMAP_OPTIONS, type RangeKey } from "./grading";
import { usePostProcessEditor } from "./usePostProcessEditor";

export function PostProcessPanel() {
  const {
    ready,
    hasStats,
    cfg,
    tab,
    setTab,
    mixerOutput,
    setMixerOutput,
    curve,
    writeBloom,
    onBloomDragStart,
    onBloomDragEnd,
    writeGrade,
    onGradeDragStart,
    onGradeDragEnd,
    writeRange,
    writeMixerCoeff,
    writeSplit,
    onTonemap,
    onExposure,
    onExposureDragStart,
    onExposureDragEnd,
    onCurveChange,
    onCurveDragStart,
    onCurveDragEnd,
  } = usePostProcessEditor();

  if (!hasStats) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="p-3.5 text-center italic text-muted-foreground">
          {ready ? "Waiting for stats…" : "Engine not ready"}
        </div>
      </div>
    );
  }

  const renderRange = (title: string, key: RangeKey, r: GradeRangeDto) => (
    <>
      <SectionBreak>{title}</SectionBreak>
      <CdlWheels
        cdl={r}
        onPatch={(patch) => writeRange(key, patch)}
        onDragStart={onGradeDragStart}
        onDragEnd={onGradeDragEnd}
      />
      <FieldRow label="Saturation">
        <NumberDrag
          value={r.saturation}
          min={0}
          max={2}
          step={0.01}
          onChange={(v) => writeRange(key, { saturation: v })}
          onDragStart={onGradeDragStart}
          onDragEnd={onGradeDragEnd}
        />
      </FieldRow>
      <FieldRow label="Contrast">
        <NumberDrag
          value={r.contrast}
          min={0}
          max={2}
          step={0.01}
          onChange={(v) => writeRange(key, { contrast: v })}
          onDragStart={onGradeDragStart}
          onDragEnd={onGradeDragEnd}
        />
      </FieldRow>
    </>
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <Tabs
        value={tab}
        onValueChange={(v) => setTab(v as "tone" | "color" | "effects")}
        className="flex min-h-0 flex-1 flex-col gap-0"
      >
        <div className="p-2.5 pb-1.5">
          <TabsList>
            <TabsTrigger value="tone">Tone</TabsTrigger>
            <TabsTrigger value="color">Color</TabsTrigger>
            <TabsTrigger value="effects">Effects</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="tone" className="min-h-0">
          <ScrollArea className="h-full min-h-0">
            <div className="flex flex-col gap-2 p-2.5 pt-1">
              <SectionBreak>Display transform</SectionBreak>
              <FieldRow label="View transform">
                <Select value={cfg.tonemap} disabled={!ready} onValueChange={onTonemap}>
                  <SelectTrigger className="h-7 text-[11px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {TONEMAP_OPTIONS.map((option) => (
                      <SelectItem key={option.value} value={option.value} className="text-[11px]">
                        {option.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </FieldRow>
              <FieldRow label="Exposure (EV)">
                <NumberDrag
                  value={cfg.exposureEv}
                  min={-8}
                  max={8}
                  step={0.05}
                  onChange={onExposure}
                  onDragStart={onExposureDragStart}
                  onDragEnd={onExposureDragEnd}
                />
              </FieldRow>
            </div>
          </ScrollArea>
        </TabsContent>

        <TabsContent value="effects" className="min-h-0">
          <ScrollArea className="h-full min-h-0">
            <div className="flex flex-col gap-2 p-2.5 pt-1">
              <ControlRow label="Bloom">
                <Tooltip>
                  <TooltipTrigger asChild>
                    <div>
                      <Switch
                        checked={cfg.bloomEnabled}
                        disabled={!ready}
                        onCheckedChange={(next) => writeBloom({ enabled: next })}
                      />
                    </div>
                  </TooltipTrigger>
                  <TooltipContent>
                    Energy-conserving scene-linear glow, composited before the tonemap
                  </TooltipContent>
                </Tooltip>
              </ControlRow>

              <FieldRow label="Intensity">
                <NumberDrag
                  value={cfg.bloomIntensity}
                  min={0}
                  max={1}
                  step={0.005}
                  onChange={(v) => writeBloom({ intensity: v })}
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>

              <FieldRow label="Scatter">
                <NumberDrag
                  value={cfg.bloomScatter}
                  min={0}
                  max={0.05}
                  step={0.0005}
                  onChange={(v) => writeBloom({ scatter: v })}
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>

              <ControlRow label="Tint">
                <ColorField
                  kind="color3"
                  value={{ x: cfg.bloomTint[0], y: cfg.bloomTint[1], z: cfg.bloomTint[2] }}
                  onChange={(patch) =>
                    writeBloom({
                      tint: [
                        patch.x ?? cfg.bloomTint[0],
                        patch.y ?? cfg.bloomTint[1],
                        patch.z ?? cfg.bloomTint[2],
                      ],
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </ControlRow>

              <SectionBreak>Lens dirt</SectionBreak>
              <FieldRow label="Intensity">
                <NumberDrag
                  value={cfg.bloomDirtIntensity}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeBloom({ dirtIntensity: v })}
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>
              <ControlRow label="Tint">
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.bloomDirtTint[0],
                    y: cfg.bloomDirtTint[1],
                    z: cfg.bloomDirtTint[2],
                  }}
                  onChange={(patch) =>
                    writeBloom({
                      dirtTint: [
                        patch.x ?? cfg.bloomDirtTint[0],
                        patch.y ?? cfg.bloomDirtTint[1],
                        patch.z ?? cfg.bloomDirtTint[2],
                      ],
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </ControlRow>

              <SectionBreak>Anamorphic streak</SectionBreak>
              <ControlRow label="Enabled">
                <Tooltip>
                  <TooltipTrigger asChild>
                    <div>
                      <Switch
                        checked={cfg.bloomAnamorphicEnabled}
                        disabled={!ready}
                        onCheckedChange={(next) =>
                          writeBloom({
                            anamorphic: {
                              enabled: next,
                              ratio: cfg.bloomAnamorphicRatio,
                              tint: cfg.bloomAnamorphicTint,
                              intensity: cfg.bloomAnamorphicIntensity,
                            },
                          })
                        }
                      />
                    </div>
                  </TooltipTrigger>
                  <TooltipContent>
                    A horizontally-squeezed streak (Wronski) added over the radial bloom
                  </TooltipContent>
                </Tooltip>
              </ControlRow>
              <FieldRow label="Streak ratio">
                <NumberDrag
                  value={cfg.bloomAnamorphicRatio}
                  min={1}
                  max={4}
                  step={0.05}
                  onChange={(v) =>
                    writeBloom({
                      anamorphic: {
                        enabled: cfg.bloomAnamorphicEnabled,
                        ratio: v,
                        tint: cfg.bloomAnamorphicTint,
                        intensity: cfg.bloomAnamorphicIntensity,
                      },
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>
              <FieldRow label="Streak intensity">
                <NumberDrag
                  value={cfg.bloomAnamorphicIntensity}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) =>
                    writeBloom({
                      anamorphic: {
                        enabled: cfg.bloomAnamorphicEnabled,
                        ratio: cfg.bloomAnamorphicRatio,
                        tint: cfg.bloomAnamorphicTint,
                        intensity: v,
                      },
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>
              <ControlRow label="Streak tint">
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.bloomAnamorphicTint[0],
                    y: cfg.bloomAnamorphicTint[1],
                    z: cfg.bloomAnamorphicTint[2],
                  }}
                  onChange={(patch) =>
                    writeBloom({
                      anamorphic: {
                        enabled: cfg.bloomAnamorphicEnabled,
                        ratio: cfg.bloomAnamorphicRatio,
                        intensity: cfg.bloomAnamorphicIntensity,
                        tint: [
                          patch.x ?? cfg.bloomAnamorphicTint[0],
                          patch.y ?? cfg.bloomAnamorphicTint[1],
                          patch.z ?? cfg.bloomAnamorphicTint[2],
                        ],
                      },
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </ControlRow>
            </div>
          </ScrollArea>
        </TabsContent>

        <TabsContent value="color" className="min-h-0">
          <ScrollArea className="h-full min-h-0">
            <div className="flex flex-col gap-2 p-2.5 pt-1">
              <SectionBreak>Global</SectionBreak>
              <FieldRow label="Temperature">
                <NumberDrag
                  value={cfg.gradeTemperature}
                  min={1667}
                  max={25000}
                  step={50}
                  onChange={(v) => writeGrade({ temperature: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Tint">
                <NumberDrag
                  value={cfg.gradeTint}
                  min={-1}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ tint: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Contrast">
                <NumberDrag
                  value={cfg.gradeContrast}
                  min={0}
                  max={2}
                  step={0.01}
                  onChange={(v) => writeGrade({ contrast: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Saturation">
                <NumberDrag
                  value={cfg.gradeSaturation}
                  min={0}
                  max={2}
                  step={0.01}
                  onChange={(v) => writeGrade({ saturation: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <CdlWheels
                cdl={{ slope: cfg.gradeSlope, offset: cfg.gradeOffset, power: cfg.gradePower }}
                onPatch={(patch) => writeGrade(patch)}
                onDragStart={onGradeDragStart}
                onDragEnd={onGradeDragEnd}
              />

              <FieldRow label="Shadows max">
                <SliderField
                  value={cfg.gradeShadowsMax}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ shadowsMax: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Highlights min">
                <SliderField
                  value={cfg.gradeHighlightsMin}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ highlightsMin: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>

              {renderRange("Shadows", "shadows", cfg.gradeShadows)}
              {renderRange("Midtones", "midtones", cfg.gradeMidtones)}
              {renderRange("Highlights", "highlights", cfg.gradeHighlights)}

              <SectionBreak>Channel mixer</SectionBreak>
              <FieldRow label="Output">
                <Select
                  value={String(mixerOutput)}
                  onValueChange={(v) => setMixerOutput(Number(v))}
                >
                  <SelectTrigger className="h-7 text-[11px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {MIXER_OUTPUTS.map((o) => (
                      <SelectItem key={o.value} value={o.value} className="text-[11px]">
                        {o.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </FieldRow>
              {(["Red", "Green", "Blue"] as const).map((label, col) => (
                <FieldRow key={label} label={label}>
                  <NumberDrag
                    value={cfg.gradeChannelMixer[mixerOutput * 3 + col]}
                    min={-2}
                    max={2}
                    step={0.01}
                    onChange={(v) => writeMixerCoeff(col, v)}
                    onDragStart={onGradeDragStart}
                    onDragEnd={onGradeDragEnd}
                  />
                </FieldRow>
              ))}

              <SectionBreak>Split tone</SectionBreak>
              <ControlRow label="Shadow">
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.gradeSplitTone.shadow[0],
                    y: cfg.gradeSplitTone.shadow[1],
                    z: cfg.gradeSplitTone.shadow[2],
                  }}
                  onChange={(patch) =>
                    writeSplit({
                      shadow: [
                        patch.x ?? cfg.gradeSplitTone.shadow[0],
                        patch.y ?? cfg.gradeSplitTone.shadow[1],
                        patch.z ?? cfg.gradeSplitTone.shadow[2],
                      ],
                    })
                  }
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </ControlRow>
              <ControlRow label="Highlight">
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.gradeSplitTone.highlight[0],
                    y: cfg.gradeSplitTone.highlight[1],
                    z: cfg.gradeSplitTone.highlight[2],
                  }}
                  onChange={(patch) =>
                    writeSplit({
                      highlight: [
                        patch.x ?? cfg.gradeSplitTone.highlight[0],
                        patch.y ?? cfg.gradeSplitTone.highlight[1],
                        patch.z ?? cfg.gradeSplitTone.highlight[2],
                      ],
                    })
                  }
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </ControlRow>
              <FieldRow label="Balance">
                <NumberDrag
                  value={cfg.gradeSplitTone.balance}
                  min={-1}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeSplit({ balance: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>

              <SectionBreak>Creative look</SectionBreak>
              <div
                className="flex items-center justify-between gap-2 rounded border border-dashed border-border bg-muted/40 px-2 py-2 text-[11px] text-muted-foreground"
                onDragOver={(e) => {
                  if (e.dataTransfer.types.includes(ASSET_DND_MIME)) {
                    e.preventDefault();
                    e.dataTransfer.dropEffect = "copy";
                  }
                }}
                onDrop={(e) => {
                  e.preventDefault();
                  const ids = assetIdsFromPayload(readAssetPayload(e.dataTransfer));
                  const id = ids[0];
                  if (id !== undefined) {
                    writeGrade({ creativeLutAsset: id });
                  }
                }}
              >
                <span className="truncate">
                  {cfg.creativeLutAsset !== "0"
                    ? `${cfg.creativeLutSize > 0 ? `${cfg.creativeLutSize}³` : "LUT"} · Tetrahedral`
                    : "Drop a .cube look"}
                </span>
                {cfg.creativeLutAsset !== "0" && (
                  <button
                    type="button"
                    className="text-[11px] text-muted-foreground hover:text-foreground"
                    onClick={() => writeGrade({ creativeLutAsset: "0" })}
                  >
                    Clear
                  </button>
                )}
              </div>
              <FieldRow label="Look intensity">
                <SliderField
                  value={cfg.creativeLutIntensity}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ creativeLutIntensity: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>

              <SectionBreak>Tone curve</SectionBreak>
              <ToneCurve
                channels={curve}
                onChange={onCurveChange}
                onDragStart={onCurveDragStart}
                onDragEnd={onCurveDragEnd}
              />

              <div className="flex justify-end pt-1">
                <button
                  type="button"
                  disabled={!ready}
                  className="rounded border border-border px-2 py-1 text-[11px] text-muted-foreground hover:text-foreground disabled:opacity-50"
                  onClick={() => {
                    void client
                      .bakeLook()
                      .then((res) => notify(`Baked look → ${res.path}`))
                      .catch((err: unknown) => notifyError(errorText(err)));
                  }}
                >
                  Bake look
                </button>
              </div>
            </div>
          </ScrollArea>
        </TabsContent>
      </Tabs>
    </div>
  );
}
