/// Operation feedback via Sonner toasts. Every user-triggered operation failure — chiefly a
/// rejected control call — surfaces here through `notifyError`, the single error location (the
/// `<Toaster />` is mounted once in `App.tsx`); non-error results use `notify`. Panel-anchored
/// *status* (the startup modal's inline name/validation line) is a local `useState` message inside
/// that panel's own DOM, not a toast.
import { toast } from "sonner";

const DEFAULT_FLASH_MS = 4000;

/// A bottom-right operation toast (Sonner), for results that have no panel of
/// their own (save/load/import/screenshot).
export function notify(message: string, ms = DEFAULT_FLASH_MS): void {
  toast(message, { duration: ms });
}

/// The error counterpart of `notify`: a bottom-right error toast for a failed operation
/// (typically a rejected control call). Surface control failures here rather than in a
/// hand-rolled per-component error banner.
export function notifyError(message: string, ms = 8000): void {
  toast.error(message, { duration: ms });
}

/// Normalize a rejected control call into a readable message. A typed `ControlError` exposes its
/// shared failure message through the standard `Error` surface.
export function errorText(err: unknown): string {
  if (typeof err === "string") {
    return err;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return String(err);
}
