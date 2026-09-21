import { useEffect, useState } from "react";
import { Copy, KeyRound, Loader2, RefreshCw, ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { copyText } from "@/lib/clipboard";
import { proxyApi } from "@/lib/api/proxy";
import type { LocalControlConfig } from "@/types/proxy";

export function LocalControlSettings() {
  const { t } = useTranslation();
  const [config, setConfig] = useState<LocalControlConfig>();
  const [token, setToken] = useState("");
  const [pending, setPending] = useState(false);

  useEffect(() => {
    let active = true;
    void proxyApi
      .getLocalControlConfig()
      .then((value) => {
        if (active) setConfig(value);
      })
      .catch((error) => {
        toast.error(String(error));
      });
    return () => {
      active = false;
    };
  }, []);

  const setEnabled = async (enabled: boolean) => {
    if (!config) return;
    setPending(true);
    try {
      const generated = await proxyApi.setLocalControlEnabled(enabled);
      setConfig({
        ...config,
        enabled,
        tokenConfigured: config.tokenConfigured || Boolean(generated),
      });
      if (generated) setToken(generated);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setPending(false);
    }
  };

  const rotateToken = async () => {
    setPending(true);
    try {
      const generated = await proxyApi.rotateLocalControlToken();
      setToken(generated);
      setConfig((current) =>
        current ? { ...current, tokenConfigured: true } : current,
      );
      toast.success(
        t("proxy.localControl.rotated", {
          defaultValue: "Control token rotated; the previous token is invalid.",
        }),
      );
    } catch (error) {
      toast.error(String(error));
    } finally {
      setPending(false);
    }
  };

  const setAllowHttp = async (allowed: boolean) => {
    if (!config) return;
    setPending(true);
    try {
      await proxyApi.setLocalControlAllowHttpLoopback(allowed);
      setConfig({ ...config, allowHttpLoopback: allowed });
    } catch (error) {
      toast.error(String(error));
    } finally {
      setPending(false);
    }
  };

  if (!config) {
    return (
      <div className="flex justify-center p-4">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card/50 p-4">
        <div className="flex items-center gap-3">
          <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-background ring-1 ring-border">
            <ShieldCheck className="h-4 w-4 text-emerald-500" />
          </div>
          <div className="space-y-1">
            <p className="text-sm font-medium">
              {t("proxy.localControl.enabled", {
                defaultValue: "Enable authenticated control API",
              })}
            </p>
            <p className="text-xs text-muted-foreground">
              {t("proxy.localControl.enabledHint", {
                defaultValue:
                  "Accept route updates only from loopback clients with this bearer token.",
              })}
            </p>
          </div>
        </div>
        <Switch
          checked={config.enabled}
          disabled={pending}
          onCheckedChange={(value) => void setEnabled(value)}
        />
      </div>

      <div className="space-y-2 rounded-xl border border-border bg-card/50 p-4">
        <div className="flex items-center justify-between gap-3">
          <div>
            <p className="flex items-center gap-2 text-sm font-medium">
              <KeyRound className="h-4 w-4" />
              {t("proxy.localControl.token", {
                defaultValue: "Control token",
              })}
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              {token
                ? t("proxy.localControl.tokenVisibleHint", {
                    defaultValue:
                      "Copy this token now. It will not be shown again.",
                  })
                : config.tokenConfigured
                  ? t("proxy.localControl.tokenHiddenHint", {
                      defaultValue:
                        "A token is configured. Rotate it to display a new token.",
                    })
                  : t("proxy.localControl.tokenMissingHint", {
                      defaultValue: "Enable the API to generate a token.",
                    })}
            </p>
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={pending}
            onClick={() => void rotateToken()}
          >
            <RefreshCw className="mr-2 h-4 w-4" />
            {t("proxy.localControl.rotate", { defaultValue: "Rotate" })}
          </Button>
        </div>
        {token ? (
          <div className="flex gap-2">
            <Input value={token} readOnly className="font-mono text-xs" />
            <Button
              variant="secondary"
              size="icon"
              aria-label={t("proxy.localControl.copy", {
                defaultValue: "Copy token",
              })}
              onClick={() => {
                void copyText(token).then(() =>
                  toast.success(
                    t("proxy.localControl.copied", {
                      defaultValue: "Control token copied",
                    }),
                  ),
                );
              }}
            >
              <Copy className="h-4 w-4" />
            </Button>
          </div>
        ) : null}
      </div>

      <div className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card/50 p-4">
        <div>
          <p className="text-sm font-medium">
            {t("proxy.localControl.allowHttp", {
              defaultValue: "Allow HTTP loopback endpoints",
            })}
          </p>
          <p className="mt-1 text-xs text-muted-foreground">
            {t("proxy.localControl.allowHttpHint", {
              defaultValue:
                "Development only. Non-loopback endpoints always require HTTPS.",
            })}
          </p>
        </div>
        <Switch
          checked={config.allowHttpLoopback}
          disabled={pending}
          onCheckedChange={(value) => void setAllowHttp(value)}
        />
      </div>
    </div>
  );
}
