<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    title: string;
    count: number;
    open?: boolean;
    actions?: Snippet;
    children: Snippet;
  };

  let {
    title,
    count,
    open = $bindable(true),
    actions,
    children,
  }: Props = $props();
</script>

<section>
  <div class="head">
    <button
      class="toggle"
      type="button"
      aria-expanded={open}
      onclick={() => (open = !open)}
    >
      <svg class="chevron" class:expanded={open} viewBox="0 0 16 16" aria-hidden="true">
        <path
          d="M6 3.5L10.5 8L6 12.5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.8"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
      <span class="title">{title}</span>
    </button>

    <span class="badge">{count}</span>

    {#if actions}
      <div class="actions">{@render actions()}</div>
    {/if}
  </div>

  {#if open}
    <ul>{@render children()}</ul>
  {/if}
</section>

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    flex: 1;
    min-width: 0;
    padding: 0.15rem 0.1rem;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: 0.8rem;
    font-weight: 600;
    letter-spacing: 0.01em;
    cursor: pointer;
    border-radius: 0.3rem;
  }

  .toggle:hover {
    color: #cfcfd4;
  }

  .toggle:hover .chevron {
    color: #cfcfd4;
  }

  .chevron {
    /* Sem width/height explícitos o SVG assume 300x150px, então precisa ser
       dimensionado aqui. */
    width: 0.75rem;
    height: 0.75rem;
    flex-shrink: 0;
    color: #8a8a92;
    transform: rotate(-90deg);
    transition: transform 0.15s ease;
  }

  .chevron.expanded {
    transform: rotate(0deg);
  }

  .badge {
    font-size: 0.7rem;
    color: #9a9aa0;
    background: #26262b;
    border-radius: 999px;
    padding: 0.05rem 0.4rem;
    min-width: 1.2rem;
    text-align: center;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 0.15rem;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
</style>