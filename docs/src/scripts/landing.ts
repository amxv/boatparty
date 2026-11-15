// Behaviour for the landing page: scroll reveals, copy buttons, the voyage
// diagram's sequence and the harbor's acquisition ladder. Everything here is
// progressive: without it the page shows every element in its final state.

const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

function initReveal() {
  const items = document.querySelectorAll<HTMLElement>(".bp-reveal");
  if (reducedMotion || !("IntersectionObserver" in window)) return;
  document.documentElement.classList.add("bp-js");
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        entry.target.classList.add("is-in");
        observer.unobserve(entry.target);
      }
    },
    { rootMargin: "0px 0px -8% 0px", threshold: 0.12 }
  );
  items.forEach((item) => observer.observe(item));
}

function initCopy() {
  document.querySelectorAll<HTMLButtonElement>("[data-copy]").forEach((button) => {
    button.addEventListener("click", async () => {
      const text = button.dataset.copy ?? "";
      try {
        await navigator.clipboard.writeText(text);
        button.textContent = "Copied";
        button.classList.add("is-copied");
      } catch {
        button.textContent = "Select";
      }
      window.setTimeout(() => {
        button.textContent = "Copy";
        button.classList.remove("is-copied");
      }, 1600);
    });
  });
}

// Step -> phase for the voyage. Steps advance every TICK ms and loop.
const TICK = 750;
const LAST_STEP = 17;

function phaseFor(step: number) {
  if (step >= 2 && step <= 3) return "mirror";
  if (step >= 4 && step <= 7) return "run";
  if (step >= 8 && step <= 9) return "copy";
  if (step >= 10) return "done";
  return "idle";
}

function initVoyage() {
  const root = document.querySelector<HTMLElement>("[data-voyage]");
  if (!root || reducedMotion) return;

  const timed = Array.from(root.querySelectorAll<HTMLElement>("[data-at]"));
  const lanes = Array.from(root.querySelectorAll<HTMLElement>("[data-lane]"));
  let step = 0;
  let timer: number | undefined;

  const render = () => {
    const phase = phaseFor(step);
    root.dataset.phase = phase;
    for (const element of timed) {
      element.classList.toggle("is-on", step >= Number(element.dataset.at));
    }
    for (const lane of lanes) {
      lane.classList.toggle("is-on", lane.dataset.lane === phase);
    }
  };

  const tick = () => {
    step = step >= LAST_STEP ? 0 : step + 1;
    render();
  };

  const start = () => {
    if (timer !== undefined) return;
    timer = window.setInterval(tick, TICK);
  };

  const stop = () => {
    if (timer === undefined) return;
    window.clearInterval(timer);
    timer = undefined;
  };

  root.classList.add("is-live");
  render();

  new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) start();
      else stop();
    }
  }).observe(root);

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) stop();
  });
}

function initHarbor() {
  const root = document.querySelector<HTMLElement>("[data-harbor]");
  if (!root) return;

  const steps = Array.from(root.querySelectorAll<HTMLElement>("[data-step]"));
  const ids = steps.map((step) => step.dataset.step ?? "");
  let index = 0;
  let paused = false;

  const show = (id: string) => {
    root.dataset.hl = id;
  };

  for (const step of steps) {
    const id = step.dataset.step ?? "";
    const hold = () => {
      paused = true;
      index = ids.indexOf(id);
      show(id);
    };
    step.addEventListener("mouseenter", hold);
    step.addEventListener("focus", hold);
    step.addEventListener("mouseleave", () => {
      paused = false;
    });
    step.addEventListener("blur", () => {
      paused = false;
    });
  }

  if (reducedMotion) return;
  window.setInterval(() => {
    if (paused || document.hidden) return;
    index = (index + 1) % ids.length;
    show(ids[index]);
  }, 2600);
}

initReveal();
initCopy();
initVoyage();
initHarbor();
