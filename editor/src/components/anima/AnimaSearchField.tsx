// The input row of AnimaSearchbar: renders committed chips as badges + a live token that becomes a
// `keyword:` chip as you type a configured verb, with an anchored suggestion popover. Keyword + value
// chip modes only.
import * as React from "react";
import { useMemo, useState } from "react";
import { X } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

import { matchChipKeyword, type ChipConfig, type ChipOption, type Token } from "./chipSearch";

interface AnimaSearchFieldProps {
  tokens: Token[];
  setTokens: (next: Token[]) => void;
  chipConfigs: ChipConfig[];
  placeholder?: string;
  inputRef: React.RefObject<HTMLInputElement | null>;
  // Commits pass the post-commit token list so the parent emits the fresh state, not its
  // stale (pre-setTokens) copy.
  onChipCommit: (tokens: Token[]) => void;
  onFreeTextCommit: (tokens: Token[]) => void;
  onBlur?: () => void;
  className?: string;
}

export function AnimaSearchField({
  tokens,
  setTokens,
  chipConfigs,
  placeholder = "",
  inputRef,
  onChipCommit,
  onFreeTextCommit,
  onBlur,
  className,
}: AnimaSearchFieldProps) {
  const [open, setOpen] = useState(false);
  const [suggestionIdx, setSuggestionIdx] = useState(0);

  const chipKeywords = useMemo(() => chipConfigs.map((c) => c.keyword), [chipConfigs]);
  const chipConfigForText = (text: string): ChipConfig | null => {
    const kw = matchChipKeyword(text, chipKeywords);
    if (kw === null) return null;
    return chipConfigs.find((c) => c.keyword === kw) ?? null;
  };

  const committed = tokens.slice(0, -1);
  const liveText = tokens[tokens.length - 1]?.text ?? "";
  const liveChip = chipConfigForText(liveText);
  const liveValue = liveChip ? liveText.slice(liveChip.keyword.length + 1) : liveText;

  const keywordQuery: string | null = (() => {
    if (!liveText.startsWith(":")) return null;
    const rest = liveText.slice(1);
    if (rest.includes(":")) return null;
    return rest;
  })();

  type SuggestionMode = "keyword" | "value" | "none";
  const suggestionMode: SuggestionMode =
    keywordQuery !== null ? "keyword" : liveChip ? "value" : "none";

  const keywordSuggestions: ChipOption[] =
    keywordQuery === null
      ? []
      : chipConfigs
          .filter((c) => {
            const q = keywordQuery.toLowerCase();
            return !q || c.keyword.toLowerCase().includes(q) || c.label.toLowerCase().includes(q);
          })
          .map((c) => ({ value: c.keyword, label: c.label }));

  const valueSuggestions: ChipOption[] = liveChip ? liveChip.options(liveValue) : [];
  const suggestions =
    suggestionMode === "keyword"
      ? keywordSuggestions
      : suggestionMode === "value"
        ? valueSuggestions
        : [];
  const showSuggestions = open && suggestions.length > 0;

  const focusEnd = () => {
    requestAnimationFrame(() => {
      const el = inputRef.current;
      if (el) {
        const len = el.value.length;
        el.setSelectionRange(len, len);
        el.focus();
      }
    });
  };

  const setLive = (text: string) => {
    const next = tokens.slice();
    next[next.length - 1] = { text };
    setTokens(next);
  };

  const pickKeyword = (opt: ChipOption) => {
    setLive(`${opt.value}:`);
    setOpen(true);
    setSuggestionIdx(0);
    focusEnd();
  };

  const pickValue = (opt: ChipOption) => {
    if (!liveChip) return;
    const next = [
      ...tokens.slice(0, -1),
      { text: `${liveChip.keyword}:${opt.value}` },
      { text: "" },
    ];
    setTokens(next);
    setSuggestionIdx(0);
    onChipCommit(next);
    focusEnd();
  };

  const commitFreeText = () => {
    if (liveText === "") return;
    const next = [...tokens, { text: "" }];
    setTokens(next);
    setOpen(false);
    onFreeTextCommit(next);
    focusEnd();
  };

  const removeToken = (index: number) => {
    const next = tokens.slice();
    next.splice(index, 1);
    setTokens(next);
    onChipCommit(next);
    focusEnd();
  };

  const backspaceCommitted = () => {
    if (committed.length === 0) return;
    const lastIdx = committed.length - 1;
    if (chipConfigForText(committed[lastIdx].text)) {
      removeToken(lastIdx); // a chip's value is opaque — delete it whole, don't reopen as raw text
    } else {
      setTokens(tokens.slice(0, -1)); // free text: reopen it as the editable live token
      focusEnd();
    }
  };

  const onInput = (e: React.ChangeEvent<HTMLInputElement>) => {
    const v = e.currentTarget.value;
    setLive(liveChip ? `${liveChip.keyword}:${v}` : v);
    setOpen(true);
    setSuggestionIdx(0);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const input = e.currentTarget;

    if (e.key === "Backspace" && input.value === "") {
      if (liveChip) {
        e.preventDefault();
        setLive(liveChip.keyword);
        setOpen(false);
        return;
      }
      if (committed.length > 0) {
        e.preventDefault();
        backspaceCommitted();
      }
      return;
    }

    if (e.key === "Enter") {
      e.preventDefault();
      if (showSuggestions) {
        if (suggestionMode === "keyword") {
          pickKeyword(keywordSuggestions[suggestionIdx] ?? keywordSuggestions[0]);
          return;
        }
        if (suggestionMode === "value") {
          pickValue(valueSuggestions[suggestionIdx] ?? valueSuggestions[0]);
          return;
        }
      }
      if (liveText !== "") {
        commitFreeText();
      } else {
        // Enter on an empty field still commits the current query (chips-only, or fully empty).
        // A commit-only searchbar (the Store) has no other way to run/persist an empty "show
        // everything" search; emit the current tokens without appending a blank free-text token.
        setOpen(false);
        onFreeTextCommit(tokens);
      }
      return;
    }

    if (
      (e.key === "Tab" || e.key === "ArrowDown" || e.key === "ArrowUp") &&
      suggestions.length > 0
    ) {
      e.preventDefault();
      if (!open) {
        setOpen(true);
        setSuggestionIdx(0);
        return;
      }
      const back = e.key === "ArrowUp" || (e.key === "Tab" && e.shiftKey);
      setSuggestionIdx((i) => (i + (back ? -1 : 1) + suggestions.length) % suggestions.length);
      return;
    }

    // Escape closes the suggestion dropdown first (and is swallowed); a second Escape, with no dropdown,
    // bubbles to the host (so a find widget can close).
    if (e.key === "Escape" && showSuggestions) {
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
    }
  };

  const renderSuggestions = () => (
    <ul role="listbox" className="py-1">
      {suggestionMode === "keyword" && (
        <li className="px-3 py-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
          Filters
        </li>
      )}
      {suggestions.map((opt, i) => (
        <li
          key={opt.value}
          role="option"
          aria-selected={i === suggestionIdx}
          className={cn(
            "cursor-pointer border-l-2 px-3 py-1.5 text-sm transition-colors",
            i === suggestionIdx
              ? "border-primary bg-primary/10 text-foreground"
              : "border-transparent text-foreground hover:bg-muted",
          )}
          onMouseDown={(e) => {
            e.preventDefault();
            if (suggestionMode === "keyword") pickKeyword(opt);
            else pickValue(opt);
          }}
          onMouseEnter={() => setSuggestionIdx(i)}
        >
          {opt.label}
        </li>
      ))}
    </ul>
  );

  return (
    <Popover open={showSuggestions} onOpenChange={setOpen}>
      <PopoverAnchor asChild>
        <div
          data-anima-search-trigger
          className={cn("inline-flex w-full flex-wrap items-center gap-1", className)}
          onClick={() => {
            setOpen(true);
            inputRef.current?.focus();
          }}
        >
          {committed.map((token, i) => {
            const cfg = chipConfigForText(token.text);
            if (cfg) {
              const raw = token.text.slice(cfg.keyword.length + 1);
              const shown = cfg.resolveLabel?.(raw) ?? raw;
              return (
                <Badge key={token.text} variant={cfg.variant ?? "secondary"} className="gap-1 pr-1">
                  {cfg.label}: {shown}
                  <button
                    type="button"
                    aria-label={`Remove ${cfg.label} filter`}
                    className="rounded-sm opacity-70 hover:opacity-100"
                    onMouseDown={(e) => {
                      e.preventDefault();
                      removeToken(i);
                    }}
                  >
                    <X className="size-3" />
                  </button>
                </Badge>
              );
            }
            return (
              <span key={token.text} className="whitespace-pre text-foreground">
                {token.text}
              </span>
            );
          })}

          <span
            className={cn(
              "inline-flex items-center",
              liveChip
                ? cn("rounded-md bg-secondary px-2 py-0.5 text-secondary-foreground")
                : "min-w-[6ch] flex-1",
            )}
          >
            {liveChip && <span className="mr-1 text-xs font-medium">{liveChip.label}:</span>}
            <input
              ref={inputRef}
              type="text"
              value={liveChip ? liveValue : liveText}
              // The placeholder is the free-text hint; while a chip keyword is active the input
              // holds that chip's value, so showing it inside the chip badge reads wrong.
              placeholder={liveChip ? "" : placeholder}
              size={liveChip ? Math.max(liveValue.length + 1, 3) : undefined}
              className={cn(
                "w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground",
                liveChip ? "min-w-[2ch] text-inherit" : undefined,
              )}
              autoComplete="off"
              spellCheck={false}
              onChange={onInput}
              onFocus={() => setOpen(true)}
              onBlur={() => onBlur?.()}
              onKeyDown={onKeyDown}
            />
          </span>
        </div>
      </PopoverAnchor>
      <PopoverContent
        align="start"
        sideOffset={4}
        className="max-h-64 max-w-md overflow-auto p-0 shadow-md"
        style={{ minWidth: "var(--radix-popover-trigger-width)" }}
        onOpenAutoFocus={(e) => e.preventDefault()}
        onCloseAutoFocus={(e) => e.preventDefault()}
        onInteractOutside={(e) => {
          const target = e.target as Node | null;
          if (
            target &&
            inputRef.current?.closest("[data-anima-search-trigger]")?.contains(target)
          ) {
            e.preventDefault();
          }
        }}
      >
        {renderSuggestions()}
      </PopoverContent>
    </Popover>
  );
}
