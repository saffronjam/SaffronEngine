// A preview image with left/right navigation. Arrow nav slides the images across on a
// bezier track; a thumbnail jump (driven from the modal) cross-fades instead. Arrows and the
// slide label appear only when the asset has more than one image.
import { useState } from "react";

import { ChevronLeft, ChevronRight } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

import { cachedImage } from "./cachedImage";
import type { GalleryImage } from "./types";
import type { GalleryNav } from "./useGallery";

// One slide, owning its own load state so a pulsing skeleton covers it until the bytes arrive
// (through the async `saffron-img://` handler) and it fades in — never a blank frame on navigate.
function Slide({
  img,
  alt,
  large,
  active,
  fade,
  tick,
}: {
  img: GalleryImage;
  alt: string;
  large: boolean;
  active: boolean;
  /** The active slide arrived via a thumbnail jump — re-key the `<img>` so its fade-in replays. */
  fade: boolean;
  tick: number;
}) {
  const [loaded, setLoaded] = useState(false);
  return (
    <div className="relative h-full w-full shrink-0">
      {loaded ? null : <div className="absolute inset-0 animate-pulse bg-muted" />}
      {/* No `loading="lazy"`: the gallery is a handful of images and the user navigates them, so
          preload every slide concurrently — a slid-to image is already there. The skeleton state
          lives on the Slide, so a fade re-key of the inner <img> doesn't flash it for a cached image. */}
      <img
        key={fade ? `fade-${tick}` : "slide"}
        src={cachedImage(large ? (img.fullUrl ?? img.url) : img.url)}
        alt={active ? alt : ""}
        onLoad={() => setLoaded(true)}
        onError={() => setLoaded(true)}
        className={cn(
          "h-full w-full object-contain transition-opacity duration-200",
          loaded ? "opacity-100" : "opacity-0",
          fade && "animate-in fade-in",
        )}
      />
    </div>
  );
}

export function GalleryViewer({
  images,
  nav,
  alt,
  showLabel = false,
  large = false,
  className,
}: {
  images: GalleryImage[];
  nav: GalleryNav;
  alt: string;
  showLabel?: boolean;
  /** Use each image's high-res `fullUrl` (the detail view); the card stays on `url`. */
  large?: boolean;
  className?: string;
}) {
  const { index, mode, tick, next, prev } = nav;
  const count = images.length;
  const multi = count > 1;
  const current = images[index];

  // Arrows must not bubble to the card (which opens the modal) or the modal backdrop.
  const stop = (fn: () => void) => (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    fn();
  };

  return (
    <div className={cn("group/gallery relative h-full w-full overflow-hidden", className)}>
      <div
        className={cn(
          "flex h-full w-full",
          mode === "slide" && "transition-transform duration-300 ease-[cubic-bezier(0.4,0,0.2,1)]",
        )}
        style={{ transform: `translateX(-${index * 100}%)` }}
      >
        {images.map((img, i) => (
          <Slide
            key={img.url}
            img={img}
            alt={alt}
            large={large}
            active={i === index}
            fade={mode === "fade" && i === index}
            tick={tick}
          />
        ))}
      </div>

      {showLabel && multi && current?.label ? (
        <Badge
          variant="secondary"
          className="absolute bottom-1 left-1/2 -translate-x-1/2 text-[10px]"
        >
          {current.label}
        </Badge>
      ) : null}
      {/* Nav reveals on hover of the gallery (`group/gallery`, for the standalone detail modal) OR the
          enclosing card (`group`) — the latter so a card control overlaying the gallery (the expand
          button, a sibling of this viewer) doesn't steal the hover and hide the arrows. */}
      {multi ? (
        <>
          <button
            type="button"
            aria-label="Previous image"
            onClick={stop(prev)}
            className="absolute top-1/2 left-1 flex size-6 -translate-y-1/2 items-center justify-center rounded-full bg-background/70 text-foreground opacity-0 group-hover/gallery:opacity-100 group-hover:opacity-100 hover:bg-background"
          >
            <ChevronLeft className="size-4" />
          </button>
          <button
            type="button"
            aria-label="Next image"
            onClick={stop(next)}
            className="absolute top-1/2 right-1 flex size-6 -translate-y-1/2 items-center justify-center rounded-full bg-background/70 text-foreground opacity-0 group-hover/gallery:opacity-100 group-hover:opacity-100 hover:bg-background"
          >
            <ChevronRight className="size-4" />
          </button>
          <div className="absolute right-1 bottom-1 rounded bg-background/70 px-1 text-[9px] text-muted-foreground opacity-0 group-hover/gallery:opacity-100 group-hover:opacity-100">
            {index + 1}/{count}
          </div>
        </>
      ) : null}
    </div>
  );
}
