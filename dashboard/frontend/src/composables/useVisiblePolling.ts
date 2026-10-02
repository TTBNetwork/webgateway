import { computed, onUnmounted, ref, type ComputedRef } from 'vue';

/**
 * State describing why the poller schedules the delay it is about to use.
 */
export interface PollTick {
    /**
     * Number of consecutive failed refreshes *before* the next attempt.
     * `0` means the previous attempt succeeded.
     */
    failureCount: number;
    /** The base interval passed to {@link useVisiblePolling}. */
    interval: number;
}

export interface UseVisiblePollingOptions {
    /**
     * The work executed on every tick. Throwing (or returning a rejected
     * promise) counts as a failure and triggers the exponential backoff.
     */
    refresh: () => unknown | Promise<unknown>;
    /** Base poll interval in milliseconds. Also the default backoff base. */
    interval: number;
    /**
     * Computes the delay before the next attempt after a successful refresh.
     * Defaults to the base `interval`. Use this to align polls to wall-clock
     * boundaries (see `QPS.vue`).
     */
    nextDelay?: (tick: PollTick) => number;
    /** Base delay for the first backoff step. Defaults to `interval`. */
    backoffBaseMs?: number;
    /** Upper bound for the backoff delay. Defaults to 5 minutes. */
    maxBackoffMs?: number;
    /** Skip the initial refresh performed by `start()`. Defaults to `false`. */
    immediate?: boolean;
    /** Invoked after every failed attempt, e.g. for logging or a toast. */
    onError?: (error: unknown, failureCount: number) => void;
}

export interface VisiblePoller {
    /** Begin polling (immediately by default). No-op when already active. */
    start: () => void;
    /** Stop polling and cancel the pending timer. */
    stop: () => void;
    /**
     * Run a refresh right away and restart the schedule from its result.
     * Never rejects: failures are reported through `onError` and the backoff.
     */
    refreshNow: () => Promise<void>;
    /** Whether `start()` has been called and `stop()` has not. */
    isActive: ComputedRef<boolean>;
    /** Whether a refresh is currently in flight. */
    isRefreshing: ComputedRef<boolean>;
    /** Consecutive failures; reset to 0 after a successful refresh. */
    failureCount: ComputedRef<number>;
}

const DEFAULT_MAX_BACKOFF_MS = 5 * 60 * 1000;

function isDocumentHidden(): boolean {
    return (
        typeof document !== 'undefined' && document.visibilityState === 'hidden'
    );
}

/**
 * Visibility-gated poller.
 *
 * - Pauses its timer while `document.visibilityState === 'hidden'`.
 * - Refreshes immediately when the page becomes visible again.
 * - Backs off exponentially on consecutive failures (capped), and resets the
 *   backoff after a success.
 * - Removes its timer and the `visibilitychange` listener on unmount.
 *
 * The composable must be called synchronously inside `setup()`.
 */
export function useVisiblePolling(
    options: UseVisiblePollingOptions,
): VisiblePoller {
    const interval = Math.max(1, options.interval);
    const backoffBase = Math.max(1, options.backoffBaseMs ?? interval);
    const maxBackoff = Math.max(
        backoffBase,
        options.maxBackoffMs ?? DEFAULT_MAX_BACKOFF_MS,
    );
    const immediate = options.immediate ?? true;

    const active = ref(false);
    const inFlight = ref(0);
    const failures = ref(0);
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;

    function clearTimer(): void {
        if (timer !== undefined) {
            clearTimeout(timer);
            timer = undefined;
        }
    }

    /** `failures` is >= 1 whenever this is called. */
    function backoffDelay(): number {
        const exponent = Math.min(failures.value - 1, 10);
        return Math.min(maxBackoff, backoffBase * 2 ** exponent);
    }

    function schedule(delay: number): void {
        clearTimer();
        if (disposed || !active.value || isDocumentHidden()) return;
        timer = setTimeout(
            () => {
                void run();
            },
            Math.max(0, delay),
        );
    }

    async function run(): Promise<void> {
        if (disposed || !active.value) return;
        clearTimer();
        if (isDocumentHidden()) return;
        inFlight.value += 1;
        try {
            await options.refresh();
            failures.value = 0;
        } catch (error) {
            failures.value += 1;
            options.onError?.(error, failures.value);
        } finally {
            inFlight.value -= 1;
        }
        if (disposed || !active.value) return;
        // Another refresh started while this one was running; let it schedule.
        if (inFlight.value > 0) return;
        // Do not schedule work that would immediately be paused again.
        if (isDocumentHidden()) return;
        const delay =
            failures.value > 0
                ? backoffDelay()
                : (options.nextDelay?.({
                      failureCount: 0,
                      interval,
                  }) ?? interval);
        schedule(delay);
    }

    function handleVisibilityChange(): void {
        if (disposed) return;
        if (isDocumentHidden()) {
            clearTimer();
            return;
        }
        if (!active.value || inFlight.value > 0) return;
        void run();
    }

    function start(): void {
        if (disposed || active.value) return;
        active.value = true;
        failures.value = 0;
        if (isDocumentHidden()) return;
        if (immediate) {
            void run();
        } else {
            schedule(interval);
        }
    }

    function stop(): void {
        active.value = false;
        clearTimer();
    }

    async function refreshNow(): Promise<void> {
        if (disposed || !active.value) return;
        await run();
    }

    if (typeof document !== 'undefined') {
        document.addEventListener('visibilitychange', handleVisibilityChange);
    }

    onUnmounted(() => {
        disposed = true;
        clearTimer();
        if (typeof document !== 'undefined') {
            document.removeEventListener(
                'visibilitychange',
                handleVisibilityChange,
            );
        }
    });

    return {
        start,
        stop,
        refreshNow,
        isActive: computed(() => active.value),
        isRefreshing: computed(() => inFlight.value > 0),
        failureCount: computed(() => failures.value),
    };
}
