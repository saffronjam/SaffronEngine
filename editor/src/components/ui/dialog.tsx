import * as React from "react";
import { XIcon } from "lucide-react";
import { Dialog as DialogPrimitive } from "radix-ui";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { withOverlayPerf } from "@/lib/overlayPerf";

function Dialog({
  perfLabel,
  onOpenChange,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Root> & { perfLabel?: string }) {
  return (
    <DialogPrimitive.Root
      data-slot="dialog"
      onOpenChange={withOverlayPerf(perfLabel, onOpenChange)}
      {...props}
    />
  );
}

function DialogTrigger({ ...props }: React.ComponentProps<typeof DialogPrimitive.Trigger>) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />;
}

function DialogPortal({ ...props }: React.ComponentProps<typeof DialogPrimitive.Portal>) {
  return <DialogPrimitive.Portal data-slot="dialog-portal" {...props} />;
}

function DialogClose({ ...props }: React.ComponentProps<typeof DialogPrimitive.Close>) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />;
}

function DialogOverlay({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Overlay>) {
  return (
    <DialogPrimitive.Overlay
      data-slot="dialog-overlay"
      className={cn(
        "fixed inset-0 z-50 bg-black/50 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:animate-in data-[state=open]:fade-in-0",
        className,
      )}
      {...props}
    />
  );
}

function DialogContent({
  className,
  children,
  showCloseButton = true,
  container,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Content> & {
  showCloseButton?: boolean;
  /// Portal target. Set it to scope the dialog to a region instead of the whole window — the
  /// content portals into `container` and, since a scoped dialog runs non-modal and brings its own
  /// backdrop (`DialogScopedOverlay`), the built-in fixed overlay is skipped.
  container?: HTMLElement | null;
}) {
  return (
    <DialogPortal data-slot="dialog-portal" container={container ?? undefined}>
      {container == null && <DialogOverlay />}
      <DialogPrimitive.Content
        data-slot="dialog-content"
        className={cn(
          "fixed top-[50%] left-[50%] z-50 grid w-full max-w-[calc(100%-2rem)] translate-x-[-50%] translate-y-[-50%] gap-4 rounded-lg border bg-background p-6 shadow-lg duration-200 outline-none data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 sm:max-w-lg",
          // A scoped dialog persists across its region's `display:none` (a hidden tab); a CSS
          // *entrance* animation would replay every time the tab is revealed, so only a window-level
          // (portal-to-body) dialog gets the open animation. The exit animation is safe either way.
          container == null &&
            "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
          className,
        )}
        {...props}
      >
        {children}
        {showCloseButton && (
          <DialogPrimitive.Close
            data-slot="dialog-close"
            className="absolute top-4 right-4 rounded-xs opacity-70 ring-offset-background transition-opacity hover:opacity-100 focus:ring-2 focus:ring-ring focus:ring-offset-2 focus:outline-hidden disabled:pointer-events-none data-[state=open]:bg-accent data-[state=open]:text-muted-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4"
          >
            <XIcon />
            <span className="sr-only">Close</span>
          </DialogPrimitive.Close>
        )}
      </DialogPrimitive.Content>
    </DialogPortal>
  );
}

/// The backdrop for a scoped (non-modal) dialog. Rendered as a `DialogPortal` child so Radix's
/// Presence mounts/unmounts it with the dialog's open state; it dims only `container` (not the whole
/// window), leaving the surrounding chrome — the main tab strip — live. Pass `onClose` to dismiss on
/// backdrop click; omit it for a backdrop that can't be clicked away. It carries only an *exit* fade,
/// no entrance one: a scoped dialog stays mounted across its region's `display:none`, and a CSS
/// entrance animation would replay every time the hidden tab is revealed (reading as a re-open).
function DialogScopedOverlay({
  open,
  container,
  onClose,
  className,
}: {
  open: boolean;
  container: HTMLElement | null;
  onClose?: () => void;
  className?: string;
}) {
  if (!container) return null;
  return (
    <DialogPortal data-slot="dialog-portal" container={container}>
      <div
        data-slot="dialog-scoped-overlay"
        data-state={open ? "open" : "closed"}
        aria-hidden
        onClick={onClose}
        className={cn(
          "absolute inset-0 z-40 bg-black/50 data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
          onClose && "cursor-pointer",
          className,
        )}
      />
    </DialogPortal>
  );
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-header"
      className={cn("flex flex-col gap-2 text-center sm:text-left", className)}
      {...props}
    />
  );
}

function DialogFooter({
  className,
  showCloseButton = false,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  showCloseButton?: boolean;
}) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn("flex flex-col-reverse gap-2 sm:flex-row sm:justify-end", className)}
      {...props}
    >
      {children}
      {showCloseButton && (
        <DialogPrimitive.Close asChild>
          <Button variant="outline">Close</Button>
        </DialogPrimitive.Close>
      )}
    </div>
  );
}

function DialogTitle({ className, ...props }: React.ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn("text-lg leading-none font-semibold", className)}
      {...props}
    />
  );
}

function DialogDescription({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  );
}

export {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogOverlay,
  DialogPortal,
  DialogScopedOverlay,
  DialogTitle,
  DialogTrigger,
};
