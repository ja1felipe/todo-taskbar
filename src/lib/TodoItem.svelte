<script lang="ts">
  import { formatCompletedAt } from "$lib/format";

  type Todo = {
    id: number;
    text: string;
    done: boolean;
    completedAt: string | null;
  };

  type Props = {
    todo: Todo;
    ontoggle: (id: number) => void;
    onremove: (id: number) => void;
  };

  let { todo, ontoggle, onremove }: Props = $props();

  let quando = $derived(formatCompletedAt(todo.completedAt));
</script>

<!-- Classes explícitas em vez de seletores de tag (`li`, `label`, `span`):
     o Svelte descarta a regra do elemento raiz deste componente, e o `li` ficava
     como `list-item`, empilhando o botão de remover abaixo do texto. -->
<li class="todo" class:done={todo.done}>
  <label class="todo-label">
    <input type="checkbox" checked={todo.done} onchange={() => ontoggle(todo.id)} />
    <span class="todo-body">
      <span class="todo-text">{todo.text}</span>
      {#if quando}
        <span class="todo-time" title="Concluída em Brasília">{quando}</span>
      {/if}
    </span>
  </label>
  <button
    class="todo-remove"
    onclick={() => onremove(todo.id)}
    title="Remover"
    aria-label={`Remover ${todo.text}`}
  >
    &times;
  </button>
</li>

<style>
  .todo {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.4rem;
    padding: 0.4rem 0.5rem;
    border-radius: 0.35rem;
    background: #232327;
  }

  .todo-label {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    flex: 1;
    min-width: 0;
    cursor: pointer;
    font-size: 0.95rem;
    line-height: 1.35;
  }

  /* Texto e horário empilham, senão o horário entraria na mesma linha e
     quebraria o wrap do texto. */
  .todo-body {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    flex: 1;
    min-width: 0;
  }

  /* Sem `nowrap`: o texto quebra em quantas linhas precisar. `anywhere` evita
     estouro horizontal quando não há espaço para quebrar a palavra. */
  .todo-text {
    overflow-wrap: anywhere;
  }

  .todo-time {
    font-size: 0.72rem;
    color: #6e6e76;
    font-variant-numeric: tabular-nums;
  }

  .todo.done .todo-text {
    text-decoration: line-through;
    color: #6e6e76;
  }

  .todo.done {
    opacity: 0.75;
  }

  .todo input[type="checkbox"] {
    /* Alinha o checkbox com a primeira linha do texto quebrado. */
    margin-top: 0.2rem;
    accent-color: #4a8cff;
    cursor: pointer;
    flex-shrink: 0;
  }

  .todo-remove {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 1.4rem;
    height: 1.4rem;
    margin-top: 0.1rem;
    border: none;
    border-radius: 0.3rem;
    background: transparent;
    color: #6e6e76;
    font-size: 1.05rem;
    line-height: 1;
    cursor: pointer;
  }

  .todo-remove:hover {
    background: #3a2026;
    color: #ff6b6b;
  }
</style>
