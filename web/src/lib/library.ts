import { writable } from 'svelte/store';

/** Bumped when the library changed behind the open views, so they load again. */
export const revision = writable(0);
