import { useEffect, useState } from "react";
import { proxyApi } from "@/lib/api/proxy";
import type {
  ProxyInteractionAttempt,
  ProxyInteractionDetail,
  ProxyInteractionRecordingConfig,
  ProxyInteractionSummary,
} from "@/types/proxy";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "sonner";

export function ProxyInteractionsPanel({
  onOpenRequestLogs,
}: {
  onOpenRequestLogs?: () => void;
}) {
  const [rows, setRows] = useState<ProxyInteractionSummary[]>([]);
  const [requestId, setRequestId] = useState("");
  const [config, setConfig] = useState<ProxyInteractionRecordingConfig>();
  const [detail, setDetail] = useState<ProxyInteractionDetail | null>(null);
  const [attempts, setAttempts] = useState<ProxyInteractionAttempt[]>([]);
  const [loading, setLoading] = useState(false);

  const refresh = async () => {
    setLoading(true);
    try {
      const [nextRows, nextConfig] = await Promise.all([
        proxyApi.listInteractions(requestId ? { requestId } : {}),
        proxyApi.getInteractionRecordingConfig(),
      ]);
      setRows(nextRows);
      setConfig(nextConfig);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const saveConfig = async (next: ProxyInteractionRecordingConfig) => {
    setConfig(next);
    try {
      await proxyApi.saveInteractionRecordingConfig(next);
      toast.success("交互记录设置已保存");
    } catch (error) {
      toast.error(String(error));
      void refresh();
    }
  };

  const openDetail = async (row: ProxyInteractionSummary) => {
    if (!window.confirm("详情可能包含已脱敏的对话内容。确认查看？")) return;
    try {
      const [nextDetail, nextAttempts] = await Promise.all([
        proxyApi.getInteraction(row.requestId),
        proxyApi.listInteractionAttempts(row.requestId),
      ]);
      setDetail(nextDetail);
      setAttempts(nextAttempts);
    } catch (error) {
      toast.error(String(error));
    }
  };

  return (
    <div className="space-y-4">
      {config && (
        <div className="rounded-xl border p-4 space-y-3">
          <div className="flex items-center justify-between">
            <div>
              <div className="font-medium">保存脱敏后的请求与响应正文</div>
              <div className="text-xs text-muted-foreground">
                默认关闭；元数据和 Provider
                尝试仍用于审计。达到期限或配额会自动清理。
              </div>
            </div>
            <Switch
              checked={config.recordBodies}
              onCheckedChange={(recordBodies) =>
                void saveConfig({ ...config, recordBodies })
              }
            />
          </div>
          <div className="grid grid-cols-3 gap-3 text-sm">
            <label>
              单请求上限（字节）
              <Input
                type="number"
                value={config.maxBodyBytes}
                onChange={(event) =>
                  setConfig({
                    ...config,
                    maxBodyBytes: Number(event.target.value),
                  })
                }
                onBlur={() => void saveConfig(config)}
              />
            </label>
            <label>
              保留天数
              <Input
                type="number"
                value={config.retentionDays}
                onChange={(event) =>
                  setConfig({
                    ...config,
                    retentionDays: Number(event.target.value),
                  })
                }
                onBlur={() => void saveConfig(config)}
              />
            </label>
            <label>
              总配额（MiB）
              <Input
                type="number"
                value={config.quotaMb}
                onChange={(event) =>
                  setConfig({ ...config, quotaMb: Number(event.target.value) })
                }
                onBlur={() => void saveConfig(config)}
              />
            </label>
          </div>
          <label className="flex items-center gap-2 text-sm">
            <Switch
              checked={config.recordRawSse}
              onCheckedChange={(recordRawSse) =>
                void saveConfig({ ...config, recordRawSse })
              }
            />
            高级：保存脱敏后重建的原始 SSE 格式
          </label>
        </div>
      )}

      <div className="flex gap-2">
        <Input
          value={requestId}
          onChange={(event) => setRequestId(event.target.value)}
          placeholder="按 Request ID 筛选"
        />
        <Button
          variant="outline"
          disabled={loading}
          onClick={() => void refresh()}
        >
          {loading ? "加载中…" : "查询"}
        </Button>
      </div>

      <div className="overflow-auto rounded-xl border">
        <table className="w-full text-sm">
          <thead className="bg-muted/40 text-left">
            <tr>
              <th className="p-3">时间</th>
              <th className="p-3">应用 / 模型</th>
              <th className="p-3">最终 Provider</th>
              <th className="p-3">状态</th>
              <th className="p-3">Hook</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr
                key={row.requestId}
                className="border-t cursor-pointer hover:bg-muted/30"
                onClick={() => void openDetail(row)}
              >
                <td className="p-3 whitespace-nowrap">
                  {new Date(row.createdAt).toLocaleString()}
                </td>
                <td className="p-3">
                  {row.appType}
                  <div className="text-xs text-muted-foreground">
                    {row.clientModel} → {row.outboundModel ?? "—"}
                  </div>
                </td>
                <td className="p-3">{row.finalProviderId ?? "—"}</td>
                <td className="p-3">{row.statusCode ?? "进行中"}</td>
                <td className="p-3">{row.hookHit ? "命中" : "—"}</td>
              </tr>
            ))}
            {!rows.length && (
              <tr>
                <td
                  className="p-6 text-center text-muted-foreground"
                  colSpan={5}
                >
                  暂无交互记录
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <Dialog
        open={Boolean(detail)}
        onOpenChange={(open) => !open && setDetail(null)}
      >
        <DialogContent className="max-w-4xl">
          <DialogHeader>
            <DialogTitle>交互详情</DialogTitle>
          </DialogHeader>
          {detail && (
            <div className="overflow-auto p-6 space-y-5 text-sm">
              <div className="grid grid-cols-2 gap-2">
                <div>
                  Request ID: <code>{detail.requestId}</code>
                </div>
                <div>
                  过期时间: {new Date(detail.retentionUntil).toLocaleString()}
                </div>
                <div>
                  模型: {detail.clientModel} → {detail.outboundModel ?? "—"}
                </div>
                <div>Provider: {detail.finalProviderId ?? "—"}</div>
              </div>
              <section>
                <h4 className="font-medium mb-2">Provider 尝试时间线</h4>
                <ol className="space-y-2">
                  {attempts.map((attempt) => (
                    <li
                      key={attempt.attemptIndex}
                      className="rounded border p-2"
                    >
                      #{attempt.attemptIndex} {attempt.providerId} ·{" "}
                      {attempt.endpointOrigin ?? "—"} ·{" "}
                      {attempt.statusCode ?? attempt.errorCode ?? "—"}
                    </li>
                  ))}
                </ol>
              </section>
              <Payload
                title="客户端输入（已脱敏）"
                value={detail.requestPayloadRedacted}
              />
              <Payload
                title="上游输入（已脱敏）"
                value={detail.upstreamRequestPayloadRedacted}
              />
              <Payload
                title="响应 / Tool Call（已脱敏）"
                value={detail.responsePayloadRedacted}
              />
              <Payload title="Hook 事件" value={detail.hookEventsJson} />
              <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground">
                <span>
                  Token、延迟与成本可按相同时间、Provider
                  和模型在请求日志中关联查看。
                </span>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={onOpenRequestLogs}
                >
                  打开请求日志
                </Button>
              </div>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}

function Payload({ title, value }: { title: string; value?: string }) {
  return (
    <section>
      <h4 className="font-medium mb-2">{title}</h4>
      <pre className="max-h-72 overflow-auto whitespace-pre-wrap rounded bg-muted p-3 text-xs">
        {value ?? "未记录正文"}
      </pre>
    </section>
  );
}
