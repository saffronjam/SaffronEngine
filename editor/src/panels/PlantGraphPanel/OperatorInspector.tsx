/// The typed parameter editor for one botanical node. Every operator's own fields are here —
/// trunk length/taper/segments, branch ratios and declination, phyllotaxis pattern and divergence,
/// tropism stimulus and plane, prune rule and threshold, roots, shells, instances, module calls —
/// and each commit is one recorded semantic edit through the panel's single write path.
import type { BotanicalElementDto, BotanicalOperatorDto } from "../../protocol";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { MAX_NODES, MAX_SEGMENTS, MAX_SIDES, MAX_WHORL, Q16, UNIT } from "./botanicalSchema";

const ELEMENTS: BotanicalElementDto[] = [
  "trunk",
  "branch",
  "root",
  "vine",
  "frond",
  "leaf",
  "needle",
  "blade",
  "flower",
  "fruit",
  "bud",
  "scar",
  "dead-part",
];

/// Element classes that carry a spine, which is what an axis operator may produce.
const AXIS_ELEMENTS: BotanicalElementDto[] = ["trunk", "branch", "root", "vine"];

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-2">
      <Label className="w-28 shrink-0 text-[11px] text-muted-foreground">{label}</Label>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

/// A number field over a scaled integer wire value: the artist edits metres or a 0..1 fraction and
/// the field writes back the integer the wire carries, clamped to the engine's bound.
function ScaledField({
  value,
  scale,
  min,
  max,
  step,
  onCommit,
}: {
  value: number;
  scale: number;
  min: number;
  max: number;
  step: number;
  onCommit: (bits: number) => void;
}) {
  return (
    <Input
      type="number"
      className="h-6 font-mono text-[11px]"
      step={step}
      value={Number((value / scale).toFixed(4))}
      onChange={(event) => {
        const next = Math.round(Math.min(max, Math.max(min, Number(event.target.value) * scale)));
        if (Number.isFinite(next) && next !== value) {
          onCommit(next);
        }
      }}
    />
  );
}

function CountField({
  value,
  min,
  max,
  onCommit,
}: {
  value: number;
  min: number;
  max: number;
  onCommit: (count: number) => void;
}) {
  return (
    <Input
      type="number"
      className="h-6 font-mono text-[11px]"
      min={min}
      max={max}
      value={value}
      onChange={(event) => {
        const next = Math.round(Math.min(max, Math.max(min, Number(event.target.value))));
        if (Number.isFinite(next) && next !== value) {
          onCommit(next);
        }
      }}
    />
  );
}

function ElementField({
  value,
  options,
  onCommit,
}: {
  value: BotanicalElementDto;
  options: BotanicalElementDto[];
  onCommit: (element: BotanicalElementDto) => void;
}) {
  return (
    <Select value={value} onValueChange={(next) => onCommit(next as BotanicalElementDto)}>
      <SelectTrigger size="sm" className="h-6 w-full text-[11px]">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((option) => (
          <SelectItem key={option} value={option} className="text-[11px]">
            {option}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

function ChoiceField<T extends string>({
  value,
  options,
  onCommit,
}: {
  value: T;
  options: readonly T[];
  onCommit: (next: T) => void;
}) {
  return (
    <Select value={value} onValueChange={(next) => onCommit(next as T)}>
      <SelectTrigger size="sm" className="h-6 w-full text-[11px]">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((option) => (
          <SelectItem key={option} value={option} className="text-[11px]">
            {option}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/// A Q15.16 metre vector, edited component by component.
function VectorField({
  value,
  onCommit,
}: {
  value: [number, number, number];
  onCommit: (next: [number, number, number]) => void;
}) {
  return (
    <div className="flex gap-1">
      {(["x", "y", "z"] as const).map((axis, index) => (
        <Input
          key={axis}
          type="number"
          step={0.1}
          className="h-6 min-w-0 flex-1 font-mono text-[11px]"
          value={Number((value[index]! / Q16).toFixed(3))}
          onChange={(event) => {
            const next: [number, number, number] = [...value];
            next[index] = Math.round(Number(event.target.value) * Q16);
            if (Number.isFinite(next[index])) {
              onCommit(next);
            }
          }}
        />
      ))}
    </div>
  );
}

export function OperatorInspector({
  operator,
  onCommit,
}: {
  operator: BotanicalOperatorDto;
  onCommit: (label: string, next: BotanicalOperatorDto) => void;
}) {
  switch (operator.kind) {
    case "trunk":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Element">
            <ElementField
              value={operator.element}
              options={AXIS_ELEMENTS}
              onCommit={(element) => onCommit("Set trunk element", { ...operator, element })}
            />
          </Row>
          <Row label="Length (m)">
            <ScaledField
              value={operator.lengthBits}
              scale={Q16}
              min={1}
              max={1024 * Q16}
              step={0.1}
              onCommit={(lengthBits) => onCommit("Set trunk length", { ...operator, lengthBits })}
            />
          </Row>
          <Row label="Base radius (m)">
            <ScaledField
              value={operator.baseRadiusBits}
              scale={Q16}
              min={1}
              max={64 * Q16}
              step={0.01}
              onCommit={(baseRadiusBits) =>
                onCommit("Set trunk radius", { ...operator, baseRadiusBits })
              }
            />
          </Row>
          <Row label="Tip taper">
            <ScaledField
              value={operator.taper[operator.taper.length - 1]?.factorBits ?? Q16}
              scale={Q16}
              min={1}
              max={Q16}
              step={0.01}
              onCommit={(factorBits) =>
                onCommit("Set trunk taper", {
                  ...operator,
                  taper: [
                    { at: 0, factorBits: operator.taper[0]?.factorBits ?? Q16 },
                    { at: UNIT, factorBits },
                  ],
                })
              }
            />
          </Row>
          <Row label="Segments">
            <CountField
              value={operator.segments}
              min={1}
              max={MAX_SEGMENTS}
              onCommit={(segments) => onCommit("Set trunk segments", { ...operator, segments })}
            />
          </Row>
        </div>
      );
    case "branch":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Element">
            <ElementField
              value={operator.element}
              options={AXIS_ELEMENTS}
              onCommit={(element) => onCommit("Set branch element", { ...operator, element })}
            />
          </Row>
          <Row label="Length ratio">
            <ScaledField
              value={operator.lengthRatio}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(lengthRatio) =>
                onCommit("Set branch length ratio", { ...operator, lengthRatio })
              }
            />
          </Row>
          <Row label="Radius ratio">
            <ScaledField
              value={operator.radiusRatio}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(radiusRatio) =>
                onCommit("Set branch radius ratio", { ...operator, radiusRatio })
              }
            />
          </Row>
          <Row label="Declination">
            <ScaledField
              value={operator.declination}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(declination) =>
                onCommit("Set branch declination", { ...operator, declination })
              }
            />
          </Row>
          <Row label="Jitter">
            <ScaledField
              value={operator.jitter}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(jitter) => onCommit("Set branch jitter", { ...operator, jitter })}
            />
          </Row>
          <Row label="Segments">
            <CountField
              value={operator.segments}
              min={1}
              max={MAX_SEGMENTS}
              onCommit={(segments) => onCommit("Set branch segments", { ...operator, segments })}
            />
          </Row>
        </div>
      );
    case "phyllotaxis":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Pattern">
            <ChoiceField
              value={operator.pattern}
              options={["alternate", "opposite", "whorled", "spiral"] as const}
              onCommit={(pattern) => onCommit("Set phyllotaxis pattern", { ...operator, pattern })}
            />
          </Row>
          <Row label="Per node">
            <CountField
              value={operator.count}
              min={1}
              max={MAX_WHORL}
              onCommit={(count) => onCommit("Set phyllotaxis count", { ...operator, count })}
            />
          </Row>
          <Row label="Nodes">
            <CountField
              value={operator.nodes}
              min={1}
              max={MAX_NODES}
              onCommit={(nodes) => onCommit("Set phyllotaxis nodes", { ...operator, nodes })}
            />
          </Row>
          <Row label="Start">
            <ScaledField
              value={operator.start}
              scale={UNIT}
              min={0}
              max={operator.end}
              step={0.01}
              onCommit={(start) => onCommit("Set phyllotaxis start", { ...operator, start })}
            />
          </Row>
          <Row label="End">
            <ScaledField
              value={operator.end}
              scale={UNIT}
              min={operator.start}
              max={UNIT}
              step={0.01}
              onCommit={(end) => onCommit("Set phyllotaxis end", { ...operator, end })}
            />
          </Row>
          <Row label="Divergence">
            <ScaledField
              value={operator.divergence}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.001}
              onCommit={(divergence) =>
                onCommit("Set phyllotaxis divergence", { ...operator, divergence })
              }
            />
          </Row>
        </div>
      );
    case "tropism":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Kind">
            <ChoiceField
              value={operator.kindOf}
              options={["phototropism", "gravitropism", "thigmotropism"] as const}
              onCommit={(kindOf) => onCommit("Set tropism kind", { ...operator, kindOf })}
            />
          </Row>
          <Row label="Strength">
            <ScaledField
              value={operator.strength}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(strength) => onCommit("Set tropism strength", { ...operator, strength })}
            />
          </Row>
          {operator.kindOf !== "gravitropism" && (
            <Row label={operator.kindOf === "phototropism" ? "Light (m)" : "Plane normal (m)"}>
              <VectorField
                value={operator.stimulusBits}
                onCommit={(stimulusBits) =>
                  onCommit("Set tropism stimulus", { ...operator, stimulusBits })
                }
              />
            </Row>
          )}
          {operator.kindOf === "thigmotropism" && (
            <Row label="Plane offset (m)">
              <ScaledField
                value={operator.planeOffsetBits}
                scale={Q16}
                min={-1024 * Q16}
                max={1024 * Q16}
                step={0.1}
                onCommit={(planeOffsetBits) =>
                  onCommit("Set obstacle plane", { ...operator, planeOffsetBits })
                }
              />
            </Row>
          )}
        </div>
      );
    case "prune":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Rule">
            <ChoiceField
              value={operator.rule}
              options={["below-height", "shorter-than", "keep-strongest"] as const}
              onCommit={(rule) => onCommit("Set prune rule", { ...operator, rule })}
            />
          </Row>
          {operator.rule !== "keep-strongest" && (
            <Row label="Threshold (m)">
              <ScaledField
                value={operator.thresholdBits}
                scale={Q16}
                min={0}
                max={1024 * Q16}
                step={0.1}
                onCommit={(thresholdBits) =>
                  onCommit("Set prune threshold", { ...operator, thresholdBits })
                }
              />
            </Row>
          )}
          {operator.rule === "keep-strongest" && (
            <Row label="Keep per parent">
              <CountField
                value={operator.count}
                min={1}
                max={MAX_WHORL}
                onCommit={(count) => onCommit("Set prune count", { ...operator, count })}
              />
            </Row>
          )}
        </div>
      );
    case "roots":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Depth ratio">
            <ScaledField
              value={operator.depthRatio}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(depthRatio) => onCommit("Set root depth", { ...operator, depthRatio })}
            />
          </Row>
          <Row label="Spread ratio">
            <ScaledField
              value={operator.spreadRatio}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(spreadRatio) => onCommit("Set root spread", { ...operator, spreadRatio })}
            />
          </Row>
          <Row label="Roots">
            <CountField
              value={operator.count}
              min={1}
              max={MAX_WHORL}
              onCommit={(count) => onCommit("Set root count", { ...operator, count })}
            />
          </Row>
        </div>
      );
    case "shell":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Material slot">
            <CountField
              value={operator.materialSlot}
              min={0}
              max={255}
              onCommit={(materialSlot) =>
                onCommit("Set shell material", { ...operator, materialSlot })
              }
            />
          </Row>
          <Row label="Sides">
            <CountField
              value={operator.sides}
              min={3}
              max={MAX_SIDES}
              onCommit={(sides) => onCommit("Set shell sides", { ...operator, sides })}
            />
          </Row>
        </div>
      );
    case "instance":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Element">
            <ElementField
              value={operator.element}
              options={ELEMENTS}
              onCommit={(element) => onCommit("Set instance element", { ...operator, element })}
            />
          </Row>
          <Row label="Material slot">
            <CountField
              value={operator.materialSlot}
              min={0}
              max={255}
              onCommit={(materialSlot) =>
                onCommit("Set instance material", { ...operator, materialSlot })
              }
            />
          </Row>
          <Row label="Size (m)">
            <ScaledField
              value={operator.sizeBits}
              scale={Q16}
              min={1}
              max={64 * Q16}
              step={0.01}
              onCommit={(sizeBits) => onCommit("Set instance size", { ...operator, sizeBits })}
            />
          </Row>
          <Row label="Roll jitter">
            <ScaledField
              value={operator.jitter}
              scale={UNIT}
              min={0}
              max={UNIT}
              step={0.01}
              onCommit={(jitter) => onCommit("Set instance jitter", { ...operator, jitter })}
            />
          </Row>
        </div>
      );
    case "drawn":
      return (
        <div className="flex flex-col gap-1">
          <Row label="Element">
            <ElementField
              value={operator.element}
              options={AXIS_ELEMENTS}
              onCommit={(element) => onCommit("Set drawn element", { ...operator, element })}
            />
          </Row>
          {operator.points.map((point, index) => (
            <Row key={`point-${index === 0 ? "base" : index}`} label={`Point ${index + 1}`}>
              <VectorField
                value={point.positionBits}
                onCommit={(positionBits) =>
                  onCommit("Move drawn point", {
                    ...operator,
                    points: operator.points.map((row, at) =>
                      at === index ? { ...row, positionBits } : row,
                    ),
                  })
                }
              />
            </Row>
          ))}
        </div>
      );
    case "module-call":
      // The call GUID is minted with the binding it names and the two are validated against each
      // other, so it is read here and rebound in the Family tab rather than typed.
      return (
        <div className="flex flex-col gap-1">
          <Row label="Call site">
            <span className="font-mono text-[11px] text-foreground">
              {operator.callGuid.slice(0, 16)}…
            </span>
          </Row>
          <p className="text-[11px] text-muted-foreground">
            Which module this call grows is bound under Family.
          </p>
        </div>
      );
    case "family":
      return (
        <p className="text-[11px] text-muted-foreground">
          The family sink takes what reaches it. It has no parameters of its own.
        </p>
      );
  }
}
