import { useEffect, useState } from "react";
import { proxyApi } from "@/lib/api/proxy";
import type { ProxyHookConfig } from "@/types/proxy";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { toast } from "sonner";

const defaults: ProxyHookConfig = {
  enabled: false,
  endpoint: "http://127.0.0.1:18765/hook",
  bearerToken: "",
  timeoutMs: 1500,
  maxPayloadBytes: 262144,
  failClosed: false,
  allowRequestReplace: false,
  sensitiveStrings: [],
};

export function ProxyHookSettings() {
  const [config, setConfig] = useState(defaults);
  const [sensitive, setSensitive] = useState("");

  useEffect(() => {
    void proxyApi
      .getHookConfig()
      .then((value) => {
        setConfig(value);
        setSensitive(value.sensitiveStrings.join("\n"));
      })
      .catch((error) => toast.error(String(error)));
  }, []);

  const save = async () => {
    try {
      await proxyApi.saveHookConfig({
        ...config,
        sensitiveStrings: sensitive
          .split("\n")
          .map((value) => value.trim())
          .filter(Boolean),
      });
      toast.success("Hook 设置已保存");
    } catch (error) {
      toast.error(String(error));
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <div className="font-medium">启用本地生命周期 Hook</div>
          <div className="text-xs text-muted-foreground">
            仅允许固定回环 HTTP 地址，默认 fail-open。请求、响应和完整 Tool Call
            会使用独立 Bearer Token 调用。
          </div>
        </div>
        <Switch
          checked={config.enabled}
          onCheckedChange={(enabled) => setConfig({ ...config, enabled })}
        />
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="text-sm">
          Hook 地址
          <Input
            value={config.endpoint}
            onChange={(event) =>
              setConfig({ ...config, endpoint: event.target.value })
            }
          />
        </label>
        <label className="text-sm">
          独立 Bearer Token
          <Input
            type="password"
            value={config.bearerToken}
            onChange={(event) =>
              setConfig({ ...config, bearerToken: event.target.value })
            }
          />
        </label>
        <label className="text-sm">
          超时（ms）
          <Input
            type="number"
            value={config.timeoutMs}
            onChange={(event) =>
              setConfig({ ...config, timeoutMs: Number(event.target.value) })
            }
          />
        </label>
        <label className="text-sm">
          Payload 上限（字节）
          <Input
            type="number"
            value={config.maxPayloadBytes}
            onChange={(event) =>
              setConfig({
                ...config,
                maxPayloadBytes: Number(event.target.value),
              })
            }
          />
        </label>
      </div>
      <label className="block text-sm">
        自定义敏感字符串（每行一个）
        <textarea
          className="mt-1 min-h-24 w-full rounded-md border bg-background p-2 font-mono text-xs"
          value={sensitive}
          onChange={(event) => setSensitive(event.target.value)}
        />
      </label>
      <div className="flex flex-wrap gap-6">
        <label className="flex items-center gap-2 text-sm">
          <Switch
            checked={config.allowRequestReplace}
            onCheckedChange={(allowRequestReplace) =>
              setConfig({ ...config, allowRequestReplace })
            }
          />
          允许 before_request 替换正文
        </label>
        <label className="flex items-center gap-2 text-sm">
          <Switch
            checked={config.failClosed}
            onCheckedChange={(failClosed) =>
              setConfig({ ...config, failClosed })
            }
          />
          Hook 异常时阻断（fail-closed）
        </label>
      </div>
      <Button onClick={() => void save()}>保存 Hook 设置</Button>
    </div>
  );
}
