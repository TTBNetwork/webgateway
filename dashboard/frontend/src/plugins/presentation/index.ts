import { ref } from 'vue';

export type PresentationType = 'success' | 'alert';

export interface Presentation {
    message: string;
    type: string;
    fadeOut: number;
    taskId: number;
    id: symbol;
}

const defaultFadeOut = 500000;

export const presentations = ref<Presentation[]>([]);
export function addPresentation(
    message: string,
    type: PresentationType,
    fadeOut?: number,
) {
    const out = fadeOut ?? defaultFadeOut;
    const id = Symbol();
    const taskId = setTimeout(() => {
        removePresentation(id);
    }, out) as any as number;
    presentations.value.push({
        id,
        message,
        type,
        fadeOut: out,
        taskId,
    });
    return id;
}

export function removePresentation(id: symbol) {
    const index = presentations.value.findIndex((p) => p.id === id);
    if (index !== -1 && presentations.value[index] != undefined) {
        clearTimeout(presentations.value[index].taskId);
        presentations.value.splice(index, 1);
    }
}

export default addPresentation;
