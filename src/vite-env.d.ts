/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}

declare module "vuetify/styles";

declare module "sortablejs" {
  export interface SortableEvent {
    item: HTMLElement;
    from: HTMLElement;
    to: HTMLElement;
    oldIndex?: number;
    newIndex?: number;
  }
  export interface SortableInstance {
    destroy(): void;
    option(name: string, value?: unknown): unknown;
  }
  interface SortableStatic {
    create(el: HTMLElement, options: Record<string, unknown>): SortableInstance;
  }
  const Sortable: SortableStatic;
  export default Sortable;
}
