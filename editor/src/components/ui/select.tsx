import * as React from "react";
import { CheckIcon, ChevronDownIcon } from "lucide-react";
import { Popover as PopoverPrimitive } from "radix-ui";

import { cn } from "@/lib/utils";
import { withOverlayPerf } from "@/lib/overlayPerf";

/// A non-modal single-select dropdown, API-compatible with the shadcn `Select` it replaces
/// (`Select` / `SelectTrigger` / `SelectValue` / `SelectContent` / `SelectItem`). It is built on a
/// Radix **Popover** (non-modal), not `SelectPrimitive`, on purpose: Radix Select unconditionally
/// mounts `react-remove-scroll` on open, which writes the `--removed-body-scroll-bar-size` custom
/// property to `<body>`; a custom-property change on an inherited ancestor forces WebKitGTK to
/// recalculate style for the whole (never-unmounted) editor DOM — the 250–500 ms overlay-open stall.
/// A non-modal Popover does none of that: no scroll-lock, no `aria-hidden` document walk, no
/// body-level custom property. Keyboard nav / typeahead / selected-item scroll are hand-rolled over
/// the option list; the selected value's label is resolved by walking the declared children, so a
/// preset value shows its label without ever opening the menu.

interface SelectContextValue {
  value: string | undefined;
  onValueChange?: (value: string) => void;
  disabled: boolean;
  open: boolean;
  setOpen: (open: boolean) => void;
  /// Declared value → label, in declaration order — derived from children (see `collectItems`).
  labels: Map<string, React.ReactNode>;
  order: string[];
  activeValue: string | null;
  setActiveValue: (value: string | null) => void;
}

const SelectContext = React.createContext<SelectContextValue | null>(null);

function useSelectContext(): SelectContextValue {
  const ctx = React.useContext(SelectContext);
  if (!ctx) {
    throw new Error("Select subcomponents must be used within <Select>");
  }
  return ctx;
}

/// Walk the declared children for `SelectItem`s (through any wrapper — fragments, `.map` arrays,
/// conditionals) and record each value's label node in declaration order. Pure and synchronous, so
/// the trigger can show a preset value's label before the menu is ever opened. Exported for tests.
export function collectItems(
  children: React.ReactNode,
  labels: Map<string, React.ReactNode> = new Map(),
  order: string[] = [],
): { labels: Map<string, React.ReactNode>; order: string[] } {
  React.Children.forEach(children, (child) => {
    if (!React.isValidElement(child)) {
      return;
    }
    if (child.type === SelectItem) {
      const props = child.props as SelectItemProps;
      if (!labels.has(props.value)) {
        labels.set(props.value, props.children);
        order.push(props.value);
      }
      return;
    }
    const nested = (child.props as { children?: React.ReactNode }).children;
    if (nested != null) {
      collectItems(nested, labels, order);
    }
  });
  return { labels, order };
}

function Select({
  value,
  onValueChange,
  children,
  disabled = false,
  open: openProp,
  defaultOpen,
  onOpenChange,
  perfLabel,
}: {
  value?: string;
  onValueChange?: (value: string) => void;
  children?: React.ReactNode;
  disabled?: boolean;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  perfLabel?: string;
}) {
  const [uncontrolledOpen, setUncontrolledOpen] = React.useState(defaultOpen ?? false);
  const open = openProp ?? uncontrolledOpen;
  const [activeValue, setActiveValue] = React.useState<string | null>(null);

  const { labels, order } = React.useMemo(() => collectItems(children), [children]);

  const timedOpenChange = React.useMemo(
    () => withOverlayPerf(perfLabel, onOpenChange),
    [perfLabel, onOpenChange],
  );

  const setOpen = React.useCallback(
    (next: boolean) => {
      if (openProp == null) {
        setUncontrolledOpen(next);
      }
      timedOpenChange?.(next);
      // On open, seed the active (keyboard-highlighted) option to the current value.
      if (next) {
        setActiveValue(value ?? null);
      }
    },
    [openProp, timedOpenChange, value],
  );

  const ctx = React.useMemo<SelectContextValue>(
    () => ({
      value,
      onValueChange,
      disabled,
      open,
      setOpen,
      labels,
      order,
      activeValue,
      setActiveValue,
    }),
    [value, onValueChange, disabled, open, setOpen, labels, order, activeValue],
  );

  return (
    <SelectContext.Provider value={ctx}>
      <PopoverPrimitive.Root data-slot="select" open={open} onOpenChange={setOpen}>
        {children}
      </PopoverPrimitive.Root>
    </SelectContext.Provider>
  );
}

function SelectTrigger({
  className,
  size = "default",
  children,
  ...props
}: React.ComponentProps<"button"> & { size?: "sm" | "default" }) {
  const ctx = useSelectContext();
  return (
    <PopoverPrimitive.Trigger asChild>
      <button
        type="button"
        role="combobox"
        aria-expanded={ctx.open}
        disabled={ctx.disabled || props.disabled}
        data-slot="select-trigger"
        data-size={size}
        className={cn(
          "flex w-fit items-center justify-between gap-2 rounded-md border border-input bg-transparent px-3 py-2 text-sm whitespace-nowrap shadow-xs transition-[color,box-shadow] outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-destructive/20 data-[placeholder]:text-muted-foreground data-[size=default]:h-9 data-[size=sm]:h-8 *:data-[slot=select-value]:line-clamp-1 *:data-[slot=select-value]:flex *:data-[slot=select-value]:items-center *:data-[slot=select-value]:gap-2 dark:bg-input/30 dark:hover:bg-input/50 dark:aria-invalid:ring-destructive/40 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 [&_svg:not([class*='text-'])]:text-muted-foreground",
          className,
        )}
        {...props}
      >
        {children}
        <ChevronDownIcon className="size-4 opacity-50" />
      </button>
    </PopoverPrimitive.Trigger>
  );
}

function SelectValue({
  placeholder,
  className,
}: {
  placeholder?: React.ReactNode;
  className?: string;
}) {
  const ctx = useSelectContext();
  const label = ctx.value != null && ctx.value !== "" ? ctx.labels.get(ctx.value) : undefined;
  const showPlaceholder = label === undefined;
  return (
    <span
      data-slot="select-value"
      data-placeholder={showPlaceholder ? "" : undefined}
      className={cn("pointer-events-none", className)}
    >
      {showPlaceholder ? placeholder : label}
    </span>
  );
}

/// Move the keyboard highlight through the option list; Enter/Space commits, Escape/outside-click
/// dismiss is handled by the Popover. Typeahead jumps by the first character of string labels.
function useListKeyDown(ctx: SelectContextValue) {
  const typeahead = React.useRef({ query: "", at: 0 });
  return React.useCallback(
    (event: React.KeyboardEvent) => {
      const { order, activeValue, setActiveValue } = ctx;
      if (order.length === 0) {
        return;
      }
      const current = activeValue != null ? order.indexOf(activeValue) : -1;
      const commit = (index: number) => {
        event.preventDefault();
        setActiveValue(order[Math.max(0, Math.min(index, order.length - 1))] ?? null);
      };
      switch (event.key) {
        case "ArrowDown":
          return commit(current + 1);
        case "ArrowUp":
          return commit(current - 1);
        case "Home":
          return commit(0);
        case "End":
          return commit(order.length - 1);
        case "Enter":
        case " ":
          if (activeValue != null) {
            event.preventDefault();
            ctx.onValueChange?.(activeValue);
            ctx.setOpen(false);
          }
          return;
        default:
          if (event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) {
            const now = performance.now();
            const t = typeahead.current;
            t.query = now - t.at > 800 ? event.key : t.query + event.key;
            t.at = now;
            const q = t.query.toLowerCase();
            const match = ctx.order.find((v) => {
              const label = ctx.labels.get(v);
              return typeof label === "string" && label.toLowerCase().startsWith(q);
            });
            if (match) {
              setActiveValue(match);
            }
          }
      }
    },
    [ctx],
  );
}

function SelectContent({
  className,
  children,
  align = "start",
  side = "bottom",
  sideOffset = 4,
  ...props
}: React.ComponentProps<typeof PopoverPrimitive.Content>) {
  const ctx = useSelectContext();
  const onKeyDown = useListKeyDown(ctx);
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        data-slot="select-content"
        role="listbox"
        align={align}
        side={side}
        sideOffset={sideOffset}
        // Focus the list itself on open so arrow keys work immediately; return focus to the trigger.
        onOpenAutoFocus={(e) => e.preventDefault()}
        onKeyDown={onKeyDown}
        tabIndex={-1}
        className={cn(
          "z-50 max-h-(--radix-popover-content-available-height) min-w-[var(--radix-popover-trigger-width)] origin-(--radix-popover-content-transform-origin) overflow-x-hidden overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md outline-none data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
          className,
        )}
        {...props}
      >
        {children}
      </PopoverPrimitive.Content>
    </PopoverPrimitive.Portal>
  );
}

interface SelectItemProps extends Omit<React.ComponentProps<"div">, "onSelect"> {
  value: string;
  disabled?: boolean;
}

function SelectItem({ className, children, value, disabled, ...props }: SelectItemProps) {
  const ctx = useSelectContext();
  const selected = ctx.value === value;
  const active = ctx.activeValue === value;
  const ref = React.useRef<HTMLDivElement>(null);

  // Keep the keyboard-highlighted option in view as it moves.
  React.useEffect(() => {
    if (active) {
      ref.current?.scrollIntoView({ block: "nearest" });
    }
  }, [active]);

  return (
    <div
      ref={ref}
      role="option"
      aria-selected={selected}
      data-slot="select-item"
      data-active={active ? "" : undefined}
      data-disabled={disabled ? "" : undefined}
      className={cn(
        "relative flex w-full cursor-default items-center gap-2 rounded-sm py-1.5 pr-8 pl-2 text-sm outline-hidden select-none data-[active]:bg-accent data-[active]:text-accent-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 [&_svg:not([class*='text-'])]:text-muted-foreground *:[span]:last:flex *:[span]:last:items-center *:[span]:last:gap-2",
        className,
      )}
      onPointerEnter={() => !disabled && ctx.setActiveValue(value)}
      onClick={() => {
        if (disabled) {
          return;
        }
        ctx.onValueChange?.(value);
        ctx.setOpen(false);
      }}
      {...props}
    >
      <span className="absolute right-2 flex size-3.5 items-center justify-center">
        {selected ? <CheckIcon className="size-4" /> : null}
      </span>
      {children}
    </div>
  );
}

export { Select, SelectContent, SelectItem, SelectTrigger, SelectValue };
