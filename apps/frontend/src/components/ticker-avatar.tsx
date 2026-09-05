import { useState } from "react";

import { useAssetLogoOverride } from "@/lib/asset-logo-registry";
import { parseOccSymbol } from "@/lib/occ-symbol";
import { cn } from "@/lib/utils";
import { Avatar, AvatarFallback, AvatarImage, getTickerLogoPaths } from "@wealthfolio/ui";

interface TickerAvatarProps {
  symbol: string;
  /** Enables an asset-scoped custom-logo override lookup. */
  assetId?: string | null;
  /** Explicit image to show first (e.g. a not-yet-saved preview). */
  src?: string;
  exchangeMic?: string | null;
  instrumentType?: string | null;
  className?: string;
  imageClassName?: string;
}

const CASH_AVATAR_LABELS: Record<string, string> = {
  USD: "$",
  CAD: "C$",
  AUD: "A$",
  NZD: "NZ$",
};

const CASH_SYMBOL_PATTERN = /^\$?CASH[-_:]([A-Z]{3})$/;

const getCashAvatarLabel = (symbol: string): string | null => {
  const normalized = symbol.trim().toUpperCase();
  if (normalized === "$CASH" || normalized === "CASH") return "$";

  const currency = CASH_SYMBOL_PATTERN.exec(normalized)?.[1];
  if (!currency) return null;

  return CASH_AVATAR_LABELS[currency] ?? currency;
};

const getFallbackAvatarLabel = (symbol: string): string => symbol.slice(0, 4);

export const TickerAvatar = ({
  symbol,
  assetId,
  src,
  exchangeMic,
  instrumentType,
  className = "size-8",
  imageClassName = "object-cover p-0",
}: TickerAvatarProps) => {
  // For OCC option symbols (e.g. "AAPL250321C00150000"), use the underlying ticker for logo
  const parsed = symbol ? parseOccSymbol(symbol) : null;
  const logoSymbol = parsed ? parsed.underlying : symbol;

  // Extract the base symbol (before any dot, hyphen, or colon) for fallback
  const baseSymbol = logoSymbol ? logoSymbol.split(/[.:-]/)[0].toUpperCase() : "";
  const fullSymbol = logoSymbol ? logoSymbol.toUpperCase() : "";

  const override = useAssetLogoOverride({ assetId, symbol: fullSymbol });
  const customSrc = src ?? override.dataUri;

  // Candidate chain: custom override → exact market logo → unsuffixed/shared logo.
  const bundledLogoUrls = getTickerLogoPaths(fullSymbol, exchangeMic, instrumentType);
  const candidates = [customSrc, ...bundledLogoUrls].filter(
    (url, index, all): url is string => !!url && all.indexOf(url) === index,
  );
  // Key the chain by identity, not by the (possibly 200 KB) data URI itself.
  const customKey = src
    ? `src:${src.length}`
    : override.dataUri
      ? (override.ref?.sha256 ?? "")
      : "";
  const chainKey = `${customKey}\n${bundledLogoUrls.join("\n")}`;
  const cashAvatarLabel = getCashAvatarLabel(fullSymbol);
  const fallbackAvatarLabel = baseSymbol ? getFallbackAvatarLabel(baseSymbol) : "•";

  // Index of the candidate currently shown; restarts from 0 whenever the chain changes
  const [failed, setFailed] = useState({ chainKey, index: 0 });
  const candidateIndex = failed.chainKey === chainKey ? failed.index : 0;
  const logoUrl = candidates[candidateIndex] ?? "";
  const logoSource = !logoUrl ? "initials" : logoUrl === customSrc ? "custom" : "bundled";

  if (cashAvatarLabel) {
    return (
      <Avatar className={cn("font-semibold", className)}>
        <AvatarFallback className="bg-primary/80 dark:bg-primary/20 text-xs font-semibold text-white">
          <span className="p-1" title={fullSymbol}>
            {cashAvatarLabel}
          </span>
        </AvatarFallback>
      </Avatar>
    );
  }

  return (
    <Avatar className={className} data-logo-source={logoSource}>
      <AvatarImage
        src={logoUrl}
        alt={fullSymbol}
        className={imageClassName}
        onLoadingStatusChange={(status) => {
          if (status === "error" && logoUrl && candidateIndex < candidates.length) {
            setFailed({ chainKey, index: candidateIndex + 1 });
          }
        }}
      />
      <AvatarFallback className="bg-primary/80 dark:bg-primary/20 font-medium text-white">
        <span
          className={cn(
            "px-0.5 leading-none",
            fallbackAvatarLabel.length >= 4 ? "text-[10px]" : "text-xs",
          )}
          title={fullSymbol}
        >
          {fallbackAvatarLabel}
        </span>
      </AvatarFallback>
    </Avatar>
  );
};
