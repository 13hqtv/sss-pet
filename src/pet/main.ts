import { mount } from "svelte";
import Pet from "./Pet.svelte";
import "./app.css";

const target = document.getElementById("app");
if (!target) throw new Error("#app element not found");

const app = mount(Pet, { target });

export default app;
