import { For, Show, createEffect, createSignal } from "solid-js";
import { render } from "solid-js/web";

function App() {
  const [count, setCount] = createSignal(0);
  const [items, setItems] = createSignal(["a", "b"]);

  createEffect(() => {
    const value = count();
    console.log("effect:", value);
    const output = document.getElementById("effect");
    if (output) output.textContent = `effect:${value}`;
  });

  return (
    <main
      id="probe"
      style="box-sizing:border-box;width:640px;height:480px;padding:24px;background:white;color:black;font-size:18px"
    >
      <button id="increment" onClick={() => setCount(count() + 1)}>
        increment
      </button>
      <span id="count">{count()}</span>
      <div id="effect" />
      <Show when={count() > 2}>
        <p id="over">over</p>
      </Show>
      <ul id="items">
        <For each={items()}>{(item) => <li>{item}</li>}</For>
      </ul>
      <button id="add" onClick={() => setItems([...items(), "c"])}>
        add
      </button>
    </main>
  );
}

render(() => <App />, document.getElementById("app")!);
