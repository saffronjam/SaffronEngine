import { PropertySection } from "../../../components/PropertySection";
import { Button } from "@/components/ui/button";
import type { Environment } from "../../../protocol";
import { NumberRow, Row, SwitchRow } from "../Row";
import type { SectionProps } from "../sectionContext";

type TimeOfDay = Environment["timeOfDay"];

/// The world clock: enable, the manual-sun override, the civil date/location the solar position is
/// derived from, and the entry point to the appearance curves.
export function ClockSection({ ctx, onOpenCurves }: SectionProps & { onOpenCurves(): void }) {
  const { env, defaults, editor, isModified } = ctx;
  const tod = env.timeOfDay;
  const patchTod = <K extends keyof TimeOfDay>(field: K, value: TimeOfDay[K]): void =>
    editor.patchBlock("timeOfDay", field, value);

  return (
    <PropertySection
      title="Clock and location"
      summary={tod.enabled ? `${(tod.timeOfDay * 24).toFixed(1)} h` : "Disabled"}
      open={ctx.sectionOpen("clock")}
      onOpenChange={(open) => ctx.setSectionOpen("clock", open)}
      modified={defaults !== null && isModified(tod, defaults.timeOfDay)}
      onReset={() => ctx.resetNested("Reset time of day", "timeOfDay")}
    >
      <SwitchRow
        label="Time of Day"
        checked={tod.enabled}
        onCheckedChange={(checked) => patchTod("enabled", checked)}
      />

      {tod.enabled ? (
        <>
          <SwitchRow
            label="Manual Sun"
            checked={tod.manualOverride}
            onCheckedChange={(checked) => patchTod("manualOverride", checked)}
          />
          <NumberRow
            label="Time (hours)"
            editor={editor}
            value={tod.timeOfDay * 24}
            min={0}
            max={24}
            step={0.01}
            onChange={(value) => patchTod("timeOfDay", value / 24)}
          />
          <NumberRow
            label="Year"
            editor={editor}
            value={tod.year}
            min={-2000}
            max={6000}
            step={1}
            onChange={(value) => patchTod("year", Math.round(value))}
          />
          <NumberRow
            label="Month"
            editor={editor}
            value={tod.month}
            min={1}
            max={12}
            step={1}
            onChange={(value) => patchTod("month", Math.round(value))}
          />
          <NumberRow
            label="Day"
            editor={editor}
            value={tod.day}
            min={1}
            max={31}
            step={1}
            onChange={(value) => patchTod("day", Math.round(value))}
          />
          <NumberRow
            label="Latitude"
            editor={editor}
            value={tod.latitude}
            min={-90}
            max={90}
            step={0.01}
            onChange={(value) => patchTod("latitude", value)}
          />
          <NumberRow
            label="Longitude"
            editor={editor}
            value={tod.longitude}
            min={-180}
            max={180}
            step={0.01}
            onChange={(value) => patchTod("longitude", value)}
          />
          <NumberRow
            label="Day Seconds"
            editor={editor}
            value={tod.dayLengthSeconds}
            min={0}
            max={86400}
            step={1}
            onChange={(value) => patchTod("dayLengthSeconds", value)}
          />
          <Row label="Automation">
            <Button
              type="button"
              variant="outline"
              size="xs"
              className="w-full"
              onClick={onOpenCurves}
            >
              Edit appearance curves
            </Button>
          </Row>
        </>
      ) : null}
    </PropertySection>
  );
}
