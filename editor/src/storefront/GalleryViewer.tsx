// A preview image with left/right navigation. Arrow nav slides the images across on a
// bezier track; a thumbnail jump (driven from the modal) cross-fades instead. Arrows and the
// slide label appear only when the asset has more than one image.
import { ChevronLeft, ChevronRight } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

import { cachedImage } from "./cachedImage";
import type { GalleryImage } from "./types";
import type { GalleryNav } from "./useGallery";

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
          <div key={img.url} className="relative h-full w-full shrink-0">
            {/* Provider thumbnails are remote URLs; the webview has no CSP restriction here. */}
            <img
              // Re-key the active slide on a fade so its fade-in animation replays.
              key={mode === "fade" && i === index ? `fade-${tick}` : "slide"}
              src={cachedImage(large ? (img.fullUrl ?? img.url) : img.url)}
              alt={i === index ? alt : ""}
              loading="lazy"
              className={cn(
                "h-full w-full object-contain",
                mode === "fade" && i === index && "animate-in fade-in duration-200",
              )}
            />
          </div>
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
      {multi ? (
        <>
          <button
            type="button"
            aria-label="Previous image"
            onClick={stop(prev)}
            className="absolute top-1/2 left-1 flex size-6 -translate-y-1/2 items-center justify-center rounded-full bg-background/70 text-foreground opacity-0 group-hover/gallery:opacity-100 hover:bg-background"
          >
            <ChevronLeft className="size-4" />
          </button>
          <button
            type="button"
            aria-label="Next image"
            onClick={stop(next)}
            className="absolute top-1/2 right-1 flex size-6 -translate-y-1/2 items-center justify-center rounded-full bg-background/70 text-foreground opacity-0 group-hover/gallery:opacity-100 hover:bg-background"
          >
            <ChevronRight className="size-4" />
          </button>
          <div className="absolute right-1 bottom-1 rounded bg-background/70 px-1 text-[9px] text-muted-foreground opacity-0 group-hover/gallery:opacity-100">
            {index + 1}/{count}
          </div>
        </>
      ) : null}
    </div>
  );
}
