export function isEmpty(value: any) {
    if (typeof value === 'string') {
        return value.trim() === '';
    } else if (Array.isArray(value)) {
        return value.length === 0;
    }
    return value === null || value === undefined || value === '';
}

export function formatDate(t: Date | string | number): string {
    const date = new Date(t);
    const year = date.getFullYear();
    const month = String(date.getMonth() + 1).padStart(2, '0');
    const day = String(date.getDate()).padStart(2, '0');
    const hours = String(date.getHours()).padStart(2, '0');
    const minutes = String(date.getMinutes()).padStart(2, '0');
    const seconds = String(date.getSeconds()).padStart(2, '0');
    return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`;
}

/** 判断错误是否来自 AbortController 取消（被新请求取代，并非真正的失败）。 */
export function isAbortError(error: unknown): boolean {
    return (
        (error instanceof DOMException && error.name === 'AbortError') ||
        (error instanceof Error && error.name === 'AbortError')
    );
}
