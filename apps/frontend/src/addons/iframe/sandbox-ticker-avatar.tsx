import { useEffect, useState } from "react";
import { Avatar, AvatarFallback, AvatarImage, cn } from "@wealthfolio/ui";

interface SandboxTickerAvatarProps {
  symbol: string;
  exchangeMic?: string | null;
  instrumentType?: string | null;
  className?: string;
}

declare global {
  // Private sandbox bridge installed by addon-sandbox-entry.tsx.
  // eslint-disable-next-line no-var
  var __wealthfolioRequestTickerLogo:
    | ((
        symbol: string,
        exchangeMic?: string | null,
        instrumentType?: string | null,
      ) => Promise<Blob | null>)
    | undefined;
}

export const SandboxTickerAvatar = ({
  symbol,
  exchangeMic,
  instrumentType,
  className = "size-8",
}: SandboxTickerAvatarProps) => {
  const baseSymbol = symbol ? symbol.split(/[.:-]/)[0].toUpperCase() : "";
  const fullSymbol = symbol ? symbol.toUpperCase() : "";
  const fallbackAvatarLabel = baseSymbol ? baseSymbol.slice(0, 4) : "•";
  const [logoUrl, setLogoUrl] = useState<string>();

  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | undefined;
    setLogoUrl(undefined);

    void (async () => {
      const requestLogo = globalThis.__wealthfolioRequestTickerLogo;
      if (!requestLogo || !fullSymbol) {
        return;
      }

      const logo = await requestLogo(fullSymbol, exchangeMic, instrumentType);
      if (cancelled) {
        return;
      }
      if (!logo || cancelled) {
        return;
      }

      objectUrl = URL.createObjectURL(logo);
      setLogoUrl(objectUrl);
    })();

    return () => {
      cancelled = true;
      if (objectUrl) {
        URL.revokeObjectURL(objectUrl);
      }
    };
  }, [exchangeMic, fullSymbol, instrumentType]);

  return (
    <Avatar className={className}>
      {logoUrl ? <AvatarImage src={logoUrl} alt={fullSymbol} className="object-cover p-0" /> : null}
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
