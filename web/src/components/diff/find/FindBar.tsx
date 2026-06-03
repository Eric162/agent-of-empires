import { useEffect, useMemo, useRef, useState } from "react";
import { findMatches, type FindMatch, type FindSide } from "./findMatches";

interface Props {
  oldContent: string;
  newContent: string;
  /** Sides to search; unified view passes both, split also both. */
  sides: FindSide[];
  /** Called with the active match (or null when none) so the host can
   *  scroll/select it in the virtualized renderer. */
  onJump: (match: FindMatch | null) => void;
  onClose: () => void;
}

/**
 * In-diff find bar. Searches the diff *model* via {@link findMatches} (not the
 * DOM), so it reaches lines the virtualized renderer hasn't mounted. Enter /
 * Shift+Enter step through matches; Esc closes.
 */
export function FindBar({ oldContent, newContent, sides, onJump, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [regex, setRegex] = useState(false);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const { matches, error } = useMemo(() => {
    try {
      return {
        matches: findMatches(oldContent, newContent, query, {
          caseSensitive,
          regex,
          sides,
        }),
        error: null as string | null,
      };
    } catch {
      return { matches: [] as FindMatch[], error: "Invalid pattern" };
    }
  }, [oldContent, newContent, query, caseSensitive, regex, sides]);

  // Clamp the active index and notify the host whenever the match set changes.
  useEffect(() => {
    if (matches.length === 0) {
      onJump(null);
      return;
    }
    const idx = ((active % matches.length) + matches.length) % matches.length;
    if (idx !== active) setActive(idx);
    onJump(matches[idx] ?? null);
    // onJump is stable from the host (useCallback); intentionally excluded.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [matches, active]);

  const step = (delta: number) => {
    if (matches.length === 0) return;
    setActive((a) => (a + delta + matches.length) % matches.length);
  };

  return (
    <div className="flex items-center gap-1 px-3 py-1.5 border-b border-surface-700/20 bg-surface-850 shrink-0">
      <input
        ref={inputRef}
        type="text"
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            step(e.shiftKey ? -1 : 1);
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
          }
        }}
        placeholder="Find in diff"
        aria-label="Find in diff"
        className="flex-1 min-w-0 bg-surface-900 border border-surface-700/40 rounded px-2 py-0.5 text-[12px] font-mono text-text-primary outline-none focus:border-brand-600"
      />
      <span
        className={`font-mono text-[11px] tabular-nums ${error ? "text-status-error" : "text-text-dim"}`}
      >
        {error
          ? error
          : matches.length === 0
            ? query
              ? "0/0"
              : ""
            : `${active + 1}/${matches.length}`}
      </span>
      <ToggleButton active={caseSensitive} onClick={() => setCaseSensitive((v) => !v)} title="Match case" label="Aa" />
      <ToggleButton active={regex} onClick={() => setRegex((v) => !v)} title="Regular expression" label=".*" />
      <IconButton onClick={() => step(-1)} title="Previous match (Shift+Enter)" disabled={matches.length === 0} label="↑" />
      <IconButton onClick={() => step(1)} title="Next match (Enter)" disabled={matches.length === 0} label="↓" />
      <IconButton onClick={onClose} title="Close (Esc)" label="✕" />
    </div>
  );
}

function ToggleButton({
  active,
  onClick,
  title,
  label,
}: {
  active: boolean;
  onClick: () => void;
  title: string;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      aria-label={title}
      title={title}
      className={`px-1.5 py-0.5 rounded text-[11px] font-mono cursor-pointer transition-colors ${
        active
          ? "bg-brand-600 text-white"
          : "text-text-dim hover:text-text-secondary"
      }`}
    >
      {label}
    </button>
  );
}

function IconButton({
  onClick,
  title,
  label,
  disabled,
}: {
  onClick: () => void;
  title: string;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      disabled={disabled}
      aria-label={title}
      className="px-1.5 py-0.5 rounded text-[11px] font-mono text-text-dim hover:text-text-secondary cursor-pointer transition-colors disabled:opacity-40 disabled:cursor-default"
    >
      {label}
    </button>
  );
}
