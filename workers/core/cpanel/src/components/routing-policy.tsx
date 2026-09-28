import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge } from "@edger/ui/components/ui/badge";
import { Button } from "@edger/ui/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@edger/ui/components/ui/dialog";
import { Input } from "@edger/ui/components/ui/input";
import { Label } from "@edger/ui/components/ui/label";
import { Skeleton } from "@edger/ui/components/ui/skeleton";
import { PlusIcon, Trash2Icon } from "@edger/ui/icons/lucide";
import * as React from "react";

import {
  can,
  deleteRoutingPolicy,
  getRoutingPolicy,
  putRoutingPolicy,
  type Principal,
  type RoutingPolicy,
  type Worker,
} from "../lib/api";
import { type TranslationKey, useI18n } from "../lib/i18n";
import {
  appFullName,
  classifyVersions,
  draftFromPolicy,
  draftOutcome,
  includedWeightSum,
  routingPolicyBlock,
  type DraftIssue,
  type RoutingDraft,
  type RowReason,
} from "../lib/routing-policy";
import { servingVersion } from "../lib/versions";

const choiceClass =
  "size-4 shrink-0 accent-primary focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring";

export function RoutingPolicyPanel({
  apiKey,
  principal,
  versions,
}: {
  apiKey: string;
  principal: Principal;
  versions: Worker[];
}) {
  const { t } = useI18n();
  const headingId = React.useId();
  if (!can(principal, "workers:read")) return null;
  const block = routingPolicyBlock(versions);
  if (block !== "ok") {
    return (
      <section aria-labelledby={headingId} className="grid gap-2 border-t p-3">
        <h2 className="font-heading text-sm font-medium" id={headingId}>
          {t("routing.title")}
        </h2>
        <p className="max-w-prose text-sm text-muted-foreground">
          {block === "core" ? t("routing.core") : t("routing.nameConflict")}
        </p>
      </section>
    );
  }
  const first = versions[0];
  if (!first) return null;
  return (
    <RoutingPolicyEditor
      apiKey={apiKey}
      name={appFullName(first)}
      principal={principal}
      versions={versions}
    />
  );
}

function RoutingPolicyEditor({
  apiKey,
  name,
  principal,
  versions,
}: {
  apiKey: string;
  name: string;
  principal: Principal;
  versions: Worker[];
}) {
  const { t } = useI18n();
  const headingId = React.useId();
  const queryClient = useQueryClient();
  const readOnly = principal.isRoot !== true;
  const queryKey = ["cpanel", "routing-policy", name] as const;
  const [notice, setNotice] = React.useState<"removed" | "saved" | null>(null);
  const query = useQuery({
    queryKey,
    queryFn: () => getRoutingPolicy(apiKey, name),
  });
  const saved = query.data ?? null;
  const formKey = `${JSON.stringify(saved)}|${versions.map((worker) => `${worker.version}:${worker.status}:${worker.visibility ?? ""}:${worker.staged ? "1" : "0"}:${worker.origin ?? ""}`).join(",")}`;

  return (
    <section aria-labelledby={headingId} className="grid gap-3 border-t p-3">
      <div className="grid max-w-prose gap-2">
        <h2 className="font-heading text-sm font-medium" id={headingId}>
          {t("routing.title")}
        </h2>
        <p className="text-sm text-pretty text-muted-foreground">{t("routing.lead")}</p>
        <p className="text-sm text-pretty text-muted-foreground">
          {t("routing.splitHint")}
        </p>
        <p className="text-sm text-pretty text-muted-foreground">{t("routing.flags")}</p>
        {readOnly && (
          <p className="text-sm text-muted-foreground">{t("routing.readOnly")}</p>
        )}
      </div>
      {notice && (
        <p className="text-sm text-foreground" role="status">
          {notice === "saved" ? t("routing.saved") : t("routing.removed")}
        </p>
      )}
      {query.isLoading ? (
        <div className="grid gap-2">
          <p role="status">{t("routing.loading")}</p>
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-24 w-full" />
        </div>
      ) : query.isError && query.data === undefined ? (
        <div className="grid gap-2">
          <p className="text-sm text-destructive" role="alert">
            {query.error instanceof Error
              ? query.error.message
              : t("routing.loadError")}
          </p>
          <Button onClick={() => void query.refetch()} type="button" variant="outline">
            {t("routing.retry")}
          </Button>
        </div>
      ) : (
        <>
          {query.isError && (
            <p className="text-sm text-destructive" role="alert">
              {query.error instanceof Error
                ? query.error.message
                : t("routing.loadError")}
            </p>
          )}
        <RoutingPolicyForm
          apiKey={apiKey}
          key={formKey}
          name={name}
          onEdit={() => setNotice(null)}
          onRemoved={() => {
            queryClient.setQueryData(queryKey, null);
            setNotice("removed");
          }}
          onSaved={(policy) => {
            queryClient.setQueryData(queryKey, policy);
            setNotice("saved");
          }}
          readOnly={readOnly}
          saved={saved}
          versions={versions}
        />
        </>
      )}
    </section>
  );
}

function RoutingPolicyForm({
  apiKey,
  name,
  onEdit,
  onRemoved,
  onSaved,
  readOnly,
  saved,
  versions,
}: {
  apiKey: string;
  name: string;
  onEdit(): void;
  onRemoved(): void;
  onSaved(policy: RoutingPolicy): void;
  readOnly: boolean;
  saved: RoutingPolicy | null;
  versions: Worker[];
}) {
  const { t } = useI18n();
  const baseId = React.useId();
  const errorId = `${baseId}-errors`;
  const [draft, setDraft] = React.useState(() => draftFromPolicy(saved, versions));
  const [attempted, setAttempted] = React.useState(false);
  const [confirm, setConfirm] = React.useState<"delete" | "save" | null>(null);
  const [pendingPolicy, setPendingPolicy] = React.useState<RoutingPolicy | null>(
    null,
  );
  const [failure, setFailure] = React.useState<string | null>(null);
  const outcome = draftOutcome(name, draft);
  const issues = outcome.kind === "invalid" ? outcome.issues : [];
  const showIssues = attempted && issues.length > 0;
  const locked = readOnly;
  const serving = servingVersion(versions);
  const includedCount = draft.weights.filter((row) => row.included).length;
  const atCap = includedCount >= 8;
  const sum = includedWeightSum(draft);
  const aside = classifyVersions(versions).excluded.filter(
    (item) => !draft.weights.some((row) => row.version === item.version && row.reason),
  );

  function touch(next: RoutingDraft) {
    setFailure(null);
    onEdit();
    setDraft(next);
  }

  const save = useMutation({
    mutationFn: (policy: RoutingPolicy) => putRoutingPolicy(apiKey, policy),
    onSuccess: (policy) => {
      setConfirm(null);
      setFailure(null);
      onSaved(policy);
    },
    onError: (error) => {
      setFailure(error instanceof Error ? error.message : t("routing.actionError"));
    },
  });
  const remove = useMutation({
    mutationFn: () => deleteRoutingPolicy(apiKey, name),
    onSuccess: () => {
      setConfirm(null);
      setFailure(null);
      onRemoved();
    },
    onError: (error) => {
      setFailure(error instanceof Error ? error.message : t("routing.actionError"));
    },
  });
  const pending = save.isPending || remove.isPending;

  function requestSave() {
    if (locked || pending) return;
    setAttempted(true);
    if (outcome.kind !== "ready") return;
    setPendingPolicy(outcome.policy);
    setFailure(null);
    setConfirm("save");
  }

  const confirmCopy =
    pendingPolicy?.tenantAccess.mode === "allowlist" && pendingPolicy.traffic
      ? t("routing.confirmSaveBoth")
      : pendingPolicy?.tenantAccess.mode === "allowlist"
        ? t("routing.confirmSaveAllowlist")
        : t("routing.confirmSaveSplit");

  return (
    <form
      className="grid gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        requestSave();
      }}
    >
      {saved ? (
        <EffectivePolicy policy={saved} />
      ) : (
        <p className="max-w-prose text-sm text-muted-foreground">
          {t("routing.empty")}{" "}
          {serving ? (
            <>
              <span className="font-mono text-foreground">{serving}</span>{" "}
              {t("routing.servesByDefault")}
            </>
          ) : (
            t("routing.noDefault")
          )}
        </p>
      )}
      {saved && outcome.kind === "absent" && (
        <p className="max-w-prose text-sm text-muted-foreground">
          {t("routing.absentHint")}
        </p>
      )}
      <fieldset className="grid gap-2" disabled={locked || pending}>
        <legend className="text-sm font-medium">{t("routing.accessLegend")}</legend>
        <label className="flex items-center gap-2 text-sm" htmlFor={`${baseId}-public`}>
          <input
            checked={draft.mode === "public"}
            className={choiceClass}
            disabled={locked || pending}
            id={`${baseId}-public`}
            name={`${baseId}-access`}
            onChange={() => touch({ ...draft, mode: "public" })}
            type="radio"
          />
          {t("routing.accessPublic")}
        </label>
        <p className="text-sm text-muted-foreground" id={`${baseId}-public-hint`}>
          {t("routing.accessPublicHint")}
        </p>
        <label
          className="flex items-center gap-2 text-sm"
          htmlFor={`${baseId}-allowlist`}
        >
          <input
            aria-describedby={`${baseId}-allowlist-hint`}
            checked={draft.mode === "allowlist"}
            className={choiceClass}
            disabled={locked || pending}
            id={`${baseId}-allowlist`}
            name={`${baseId}-access`}
            onChange={() => touch({ ...draft, mode: "allowlist" })}
            type="radio"
          />
          {t("routing.accessAllowlist")}
        </label>
        <p className="text-sm text-muted-foreground" id={`${baseId}-allowlist-hint`}>
          {t("routing.accessAllowlistHint")}
        </p>
        {draft.mode === "allowlist" && (
          <div className="grid gap-2">
            {draft.tenants.map((slug, index) => {
              const invalid =
                showIssues &&
                (slugIssue(issues, slug) ||
                  (slug === "" &&
                    issues.some((issue) => issue.code === "tenant-required")));
              return (
                <div className="grid gap-1.5" key={`${baseId}-tenant-${index}`}>
                  <Label htmlFor={`${baseId}-tenant-${index}`}>
                    {t("routing.tenantLabel")} {index + 1}
                  </Label>
                  <div className="flex items-center gap-2">
                    <Input
                      aria-describedby={showIssues ? errorId : undefined}
                      aria-invalid={invalid || undefined}
                      autoCapitalize="off"
                      disabled={locked || pending}
                      autoCorrect="off"
                      id={`${baseId}-tenant-${index}`}
                      onChange={(event) => {
                        const tenants = draft.tenants.map((current, currentIndex) =>
                          currentIndex === index ? event.target.value : current,
                        );
                        touch({ ...draft, tenants });
                      }}
                      spellCheck={false}
                      value={slug}
                    />
                    {draft.tenants.length > 1 && (
                      <Button
                        aria-label={
                          slug
                            ? `${t("routing.tenantRemove")} ${slug}`
                            : `${t("routing.tenantRemove")} ${index + 1}`
                        }
                        onClick={() =>
                          touch({
                            ...draft,
                            tenants: draft.tenants.filter(
                              (_current, currentIndex) => currentIndex !== index,
                            ),
                          })
                        }
                        type="button"
                        variant="outline"
                      >
                        <Trash2Icon />
                        {t("routing.tenantRemove")}
                      </Button>
                    )}
                  </div>
                </div>
              );
            })}
            <Button
              onClick={() => touch({ ...draft, tenants: [...draft.tenants, ""] })}
              type="button"
              variant="outline"
            >
              <PlusIcon />
              {t("routing.tenantAdd")}
            </Button>
          </div>
        )}
      </fieldset>
      <fieldset className="grid gap-2" disabled={locked || pending}>
        <legend className="text-sm font-medium">{t("routing.splitLegend")}</legend>
        <label className="flex items-center gap-2 text-sm" htmlFor={`${baseId}-split`}>
          <input
            checked={draft.split}
            className={choiceClass}
            disabled={locked || pending}
            id={`${baseId}-split`}
            onChange={(event) => touch({ ...draft, split: event.target.checked })}
            type="checkbox"
          />
          {t("routing.splitEnable")}
        </label>
        <p className="text-sm text-muted-foreground">{t("routing.splitKeepDefault")}</p>
        {draft.split && (
          <div className="grid gap-2">
            <p className="text-sm text-muted-foreground">{t("routing.weightCount")}</p>
            {draft.weights.length === 0 && (
              <p className="text-sm text-muted-foreground">{t("routing.noEligible")}</p>
            )}
            <ul className="grid gap-2">
              {draft.weights.map((row, index) => {
                const includeId = `${baseId}-include-${index}`;
                const weightId = `${baseId}-weight-${index}`;
                const isDefault = row.version === serving && row.reason === null;
                return (
                  <li className="grid gap-2 sm:grid-cols-2 sm:items-end" key={row.version}>
                    <div className="grid gap-1">
                      <div className="flex items-center gap-2">
                        <input
                          checked={row.included}
                          className={choiceClass}
                          disabled={locked || pending || (atCap && !row.included)}
                          id={includeId}
                          onChange={(event) =>
                            touch({
                              ...draft,
                              weights: draft.weights.map((current) =>
                                current.version === row.version
                                  ? { ...current, included: event.target.checked }
                                  : current,
                              ),
                            })
                          }
                          type="checkbox"
                        />
                        <label className="font-mono text-sm" htmlFor={includeId}>
                          {row.version}
                        </label>
                        {isDefault && (
                          <Badge variant="secondary">{t("routing.defaultVersion")}</Badge>
                        )}
                      </div>
                      {row.reason && (
                        <p className="text-sm text-muted-foreground">
                          {t(reasonKey(row.reason))}
                        </p>
                      )}
                    </div>
                    <div className="grid gap-1.5">
                      <Label htmlFor={weightId}>
                        {t("routing.versionWeight")} {row.version}
                      </Label>
                      <Input
                        aria-describedby={showIssues ? errorId : undefined}
                        aria-invalid={
                          (showIssues &&
                            row.included &&
                            weightIssue(issues, row.version)) ||
                          undefined
                        }
                        disabled={!row.included || locked || pending}
                        id={weightId}
                        inputMode="numeric"
                        onChange={(event) =>
                          touch({
                            ...draft,
                            weights: draft.weights.map((current) =>
                              current.version === row.version
                                ? { ...current, weight: event.target.value }
                                : current,
                            ),
                          })
                        }
                        value={row.weight}
                      />
                    </div>
                  </li>
                );
              })}
            </ul>
            <p aria-live="polite" className="text-sm text-muted-foreground">
              {t("routing.weightsTotal")} {sum === null ? "—" : sum}
            </p>
          </div>
        )}
      </fieldset>
      {draft.split && aside.length > 0 && (
        <div className="grid gap-1">
          <h3 className="text-sm font-medium">{t("routing.notOffered")}</h3>
          <ul className="grid gap-1 text-sm text-muted-foreground">
            {aside.map((item) => (
              <li key={item.version}>
                <span className="font-mono text-foreground">{item.version}</span>
                {" — "}
                {t(reasonKey(item.reason))}
              </li>
            ))}
          </ul>
        </div>
      )}
      {showIssues && (
        <ul className="grid gap-1 text-sm text-destructive" id={errorId} role="alert">
          {issues.map((issue) => (
            <li key={issueKey(issue)}>{issueText(issue, t)}</li>
          ))}
        </ul>
      )}
      {failure && confirm === null && (
        <p className="text-sm text-destructive" role="alert">
          {failure}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <Button disabled={locked || pending} type="submit">
          {t("routing.save")}
        </Button>
        <Button
          disabled={locked || pending || saved === null}
          onClick={() => {
            setFailure(null);
            setConfirm("delete");
          }}
          type="button"
          variant="destructive"
        >
          {t("routing.remove")}
        </Button>
      </div>
      <Dialog
        onOpenChange={(open) => {
          if (!open && !pending) setConfirm(null);
        }}
        open={confirm === "save"}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("routing.confirmSaveTitle")}</DialogTitle>
            <DialogDescription>{confirmCopy}</DialogDescription>
          </DialogHeader>
          {failure && (
            <p className="text-sm text-destructive" role="alert">
              {failure}
            </p>
          )}
          <DialogFooter>
            <Button
              disabled={pending}
              onClick={() => setConfirm(null)}
              type="button"
              variant="outline"
            >
              {t("routing.cancel")}
            </Button>
            <Button
              disabled={pending || pendingPolicy === null}
              onClick={() => {
                if (pendingPolicy) save.mutate(pendingPolicy);
              }}
              type="button"
            >
              {save.isPending ? t("routing.saving") : t("routing.confirmApply")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        onOpenChange={(open) => {
          if (!open && !pending) setConfirm(null);
        }}
        open={confirm === "delete"}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("routing.confirmDeleteTitle")}</DialogTitle>
            <DialogDescription>{t("routing.confirmDeleteBody")}</DialogDescription>
          </DialogHeader>
          {failure && (
            <p className="text-sm text-destructive" role="alert">
              {failure}
            </p>
          )}
          <DialogFooter>
            <Button
              disabled={pending}
              onClick={() => setConfirm(null)}
              type="button"
              variant="outline"
            >
              {t("routing.cancel")}
            </Button>
            <Button
              disabled={pending}
              onClick={() => remove.mutate()}
              type="button"
              variant="destructive"
            >
              {remove.isPending ? t("routing.removing") : t("routing.confirmRemove")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </form>
  );
}

function EffectivePolicy({ policy }: { policy: RoutingPolicy }) {
  const { t } = useI18n();
  return (
    <div className="grid gap-2">
      <h3 className="text-sm font-medium">{t("routing.effective")}</h3>
      <dl className="grid gap-2 text-sm">
        <div className="grid gap-1">
          <dt className="font-medium">{t("routing.accessLegend")}</dt>
          <dd className="text-muted-foreground">
            {policy.tenantAccess.mode === "public" ? (
              t("routing.accessPublic")
            ) : (
              <ul className="grid gap-1">
                {policy.tenantAccess.tenants.map((slug) => (
                  <li className="font-mono text-foreground" key={slug}>
                    {slug}
                  </li>
                ))}
              </ul>
            )}
          </dd>
        </div>
        <div className="grid gap-1">
          <dt className="font-medium">{t("routing.splitLegend")}</dt>
          <dd className="text-muted-foreground">
            {policy.traffic ? (
              <ul className="grid gap-1">
                {policy.traffic.versions.map((entry) => (
                  <li key={entry.version}>
                    <span className="font-mono text-foreground">{entry.version}</span>{" "}
                    {entry.weight}
                  </li>
                ))}
              </ul>
            ) : (
              t("routing.splitKeepDefault")
            )}
          </dd>
        </div>
      </dl>
    </div>
  );
}

function reasonKey(reason: RowReason): TranslationKey {
  switch (reason) {
    case "core":
      return "routing.reason.core";
    case "disabled":
      return "routing.reason.disabled";
    case "internal":
      return "routing.reason.internal";
    case "missing":
      return "routing.reason.missing";
    case "staged":
      return "routing.reason.staged";
  }
}

function issueKey(issue: DraftIssue): string {
  switch (issue.code) {
    case "tenant-duplicate":
    case "tenant-invalid":
      return `${issue.code}:${issue.slug}`;
    case "version-ineligible":
    case "weight-invalid":
      return `${issue.code}:${issue.version}`;
    case "weight-sum":
      return `${issue.code}:${issue.sum}`;
    default:
      return issue.code;
  }
}

function issueText(
  issue: DraftIssue,
  t: (key: TranslationKey) => string,
): string {
  switch (issue.code) {
    case "no-eligible":
      return t("routing.noEligible");
    case "tenant-duplicate":
      return `${issue.slug}: ${t("routing.tenantDuplicate")}`;
    case "tenant-invalid":
      return `${issue.slug}: ${t("routing.tenantInvalid")}`;
    case "tenant-required":
      return t("routing.tenantRequired");
    case "version-ineligible":
      return `${issue.version}: ${t("routing.versionIneligible")}`;
    case "weight-count":
      return t("routing.weightCount");
    case "weight-invalid":
      return `${issue.version}: ${t("routing.weightInvalid")}`;
    case "weight-sum":
      return `${t("routing.weightSum")} (${t("routing.weightsTotal")} ${issue.sum}).`;
  }
}

function slugIssue(issues: readonly DraftIssue[], slug: string): boolean {
  return issues.some(
    (issue) =>
      (issue.code === "tenant-invalid" || issue.code === "tenant-duplicate") &&
      issue.slug === slug,
  );
}

function weightIssue(issues: readonly DraftIssue[], version: string): boolean {
  return issues.some(
    (issue) =>
      (issue.code === "weight-invalid" ||
        issue.code === "version-ineligible" ||
        issue.code === "weight-sum") &&
      (issue.code === "weight-sum" || issue.version === version),
  );
}
