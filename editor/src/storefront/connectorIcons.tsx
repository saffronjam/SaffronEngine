// Per-provider square logos, rendered as inline SVG rather than <img> so the webview draws them as
// crisp vectors at the displayed size — an <img src=svg> is rasterized at the file's intrinsic size
// and bilinearly scaled, which looks jagged. A connector with no mapped asset falls back to an
// initials monogram.
import { cn } from "@/lib/utils";

import ambientcg from "../assets/connectors/ambientcg.svg?raw";
import polyhaven from "../assets/connectors/polyhaven.svg?raw";
import polyPizza from "../assets/connectors/poly-pizza.svg?raw";
import sketchfab from "../assets/connectors/sketchfab.svg?raw";

// Strip the XML declaration / DOCTYPE so the markup is valid to inline into an HTML element.
function inlineable(svg: string): string {
  return svg
    .replace(/<\?xml[\s\S]*?\?>/i, "")
    .replace(/<!DOCTYPE[\s\S]*?>/i, "")
    .trim();
}

const ICONS: Record<string, string> = {
  polyhaven: inlineable(polyhaven),
  ambientcg: inlineable(ambientcg),
  "poly-pizza": inlineable(polyPizza),
  sketchfab: inlineable(sketchfab),
};

function initials(name: string): string {
  return name
    .split(/\s+/)
    .map((word) => word[0] ?? "")
    .join("")
    .slice(0, 2)
    .toUpperCase();
}

export function ProviderLogo({
  connectorId,
  name,
  className,
}: {
  connectorId: string;
  name: string;
  className?: string;
}) {
  const markup = ICONS[connectorId];
  if (markup) {
    return (
      <span
        role="img"
        aria-label={name}
        className={cn("inline-flex [&>svg]:size-full", className)}
        // Trusted, bundled brand assets — inlined so the webview renders crisp vectors.
        // eslint-disable-next-line react/no-danger
        dangerouslySetInnerHTML={{ __html: markup }}
      />
    );
  }
  return (
    <div
      className={cn(
        "flex items-center justify-center bg-muted text-sm font-semibold text-foreground",
        className,
      )}
    >
      {initials(name)}
    </div>
  );
}
