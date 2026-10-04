import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useTauriEvent } from "../../hooks/useTauriEvent";
import { api } from "../../api";
import type {
  PermissionRevertedEvent,
  ProjectPermission,
} from "../../api/permission";
import { i18n } from "../../lib/i18n";
import { renderError } from "../../lib/i18n-error";
import { useAppState } from "../../providers/AppStateProvider";

/**
 * One project's permission state, kept fresh, and the four verbs that
 * change it. Every verb replaces the state with what the backend
 * returns, so the panel shows what landed rather than what was asked.
 */
export function useProjectPermission(
  projectPath: string,
  onError?: (msg: string) => void,
) {
  const { t } = useTranslation("projects");
  const { pushToast } = useAppState();
  const [perm, setPerm] = useState<ProjectPermission | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  // Bumped to force a refetch — by the `permission-reverted` event
  // (a grant lapsed on the orchestrator's tick) without a manual
  // reselect.
  const [reloadTick, setReloadTick] = useState(0);

  const fail = useCallback(
    (msg: string) => {
      if (onError) onError(msg);
      else pushToast("error", msg);
    },
    [onError, pushToast],
  );

  // Single-project fetch (not the full-tree `permissionList`). Re-runs
  // on `projectPath` change and on `reloadTick` bumps; the `cancelled`
  // guard keeps a slow in-flight fetch from clobbering a newer one.
  //
  // Stale-while-revalidate: don't flip loading=true on refetches when
  // we already have data. The panel's loading branch then only fires
  // on first mount; later refetches swap the content in atomically.
  // Same defect-class as the Env panel one step over.
  useEffect(() => {
    let cancelled = false;
    api
      .permissionGet(projectPath)
      .then((p) => {
        if (!cancelled) setPerm(p);
      })
      .catch((e) => {
        if (!cancelled) {
          fail(renderError(e, i18n.t("projects:permission.loadFailedScope")));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectPath, reloadTick, fail]);

  // The orchestrator drops lapsed grants on its 5-min tick and emits
  // `permission-reverted`. Refetch when that fires for THIS project so
  // the countdown doesn't linger on a stale "expired".
  useTauriEvent<PermissionRevertedEvent>("permission-reverted", (e) => {
    if (e.payload.projectPath === projectPath) {
      setReloadTick((n) => n + 1);
    }
  });

  const run = useCallback(
    async (
      action: () => Promise<ProjectPermission>,
      okToast: string,
      failScope: string,
    ) => {
      setBusy(true);
      try {
        setPerm(await action());
        pushToast("info", okToast);
      } catch (e) {
        fail(renderError(e, failScope));
      } finally {
        setBusy(false);
      }
    },
    [pushToast, fail],
  );

  const grant = (durationSecs: number | null) =>
    run(
      () => api.permissionGrant(projectPath, durationSecs),
      durationSecs == null
        ? t("permission.grantStickyToast")
        : t("permission.grantTimedToast"),
      t("permission.grantFailedScope"),
    );

  const extend = (durationSecs: number | null) =>
    run(
      () => api.permissionExtend(projectPath, durationSecs),
      durationSecs == null
        ? t("permission.neverExpireToast")
        : t("permission.extendedToast"),
      t("permission.extendFailedScope"),
    );

  const revert = () =>
    run(
      () => api.permissionRevert(projectPath),
      t("permission.revertedToast"),
      t("permission.revertFailedScope"),
    );

  const clearIgnored = () =>
    run(
      () => api.permissionClearIgnored(projectPath),
      t("permission.clearIgnoredToast"),
      t("permission.clearIgnoredFailedScope"),
    );

  return { perm, loading, busy, grant, extend, revert, clearIgnored };
}
