"use strict";

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

// Truncating, never rounding up. A figure that promises more space than the
// removal frees is a bug here for the same reason it is one in format::bytes.
function bytes(value) {
  let size = Number(value);
  let unit = 0;
  while (size >= 1024 && unit < UNITS.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const shown = unit === 0 ? String(Math.floor(size)) : (Math.floor(size * 10) / 10).toFixed(1);
  return `${shown} ${UNITS[unit]}`;
}

// `UsageEvidence` is externally tagged on `state`; the two dated variants are
// the only ones carrying a timestamp.
function lastUse(usage) {
  switch (usage.state) {
    case "used":
    case "used-from-home":
      return new Date(usage.at * 1000).toISOString().slice(0, 10);
    case "never-since-install":
      return "never";
    case "no-witness":
      return "no evidence";
    case "atime-disabled":
      return "off";
    case "not-probed":
      return "not probed";
    default:
      return "";
  }
}

// `Origin` serialises as {kind: "repository", name: "extra"} | {kind: "foreign"}
// | {kind: "unknown"} — the same three answers Origin::label gives.
function originLabel(origin) {
  switch (origin.kind) {
    case "repository":
      return origin.name;
    case "foreign":
      return "aur/local";
    case "unknown":
      return "?";
    default:
      return "";
  }
}

function isOrphan(entry) {
  return entry.package.reason === "dependency" && entry.facts.required_by.length === 0;
}

const SORTS = {
  name: (entry) => entry.package.name,
  size: (entry) => -Number(entry.package.size),
  reclaimable: (entry) => -Number(entry.facts.reclaimable),
  reason: (entry) => entry.package.reason,
  origin: (entry) => originLabel(entry.facts.origin),
  usage: (entry) => entry.facts.usage.at ?? 0,
};

let report = null;
let sortKey = "size";

function escapeHtml(text) {
  return String(text).replace(
    /[&<>"']/g,
    (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character],
  );
}

function tile(label, value) {
  return `<div class="tile"><div class="value">${escapeHtml(value)}</div><div class="label">${escapeHtml(label)}</div></div>`;
}

function renderSummary(summary) {
  document.getElementById("summary").innerHTML = [
    tile("packages", summary.packages),
    tile("installed", bytes(summary.installed_bytes)),
    tile("orphans", `${summary.orphans} · ${bytes(summary.orphan_bytes)}`),
    tile("foreign", summary.foreign),
    tile("never used", `${summary.never_used} · ${bytes(summary.never_used_bytes)}`),
    tile("cleanup targets", bytes(summary.target_bytes)),
  ].join("");
}

function renderTable() {
  if (report === null) {
    return;
  }

  const needle = document.getElementById("search").value.trim().toLowerCase();
  const onlyOrphans = document.getElementById("only-orphans").checked;
  const onlyForeign = document.getElementById("only-foreign").checked;
  const onlyUnused = document.getElementById("only-unused").checked;

  const rows = report.inventory.entries
    .filter((entry) => {
      const pkg = entry.package;
      if (
        needle &&
        !pkg.name.toLowerCase().includes(needle) &&
        !pkg.description.toLowerCase().includes(needle)
      ) {
        return false;
      }
      if (onlyOrphans && !isOrphan(entry)) {
        return false;
      }
      if (onlyForeign && entry.facts.origin.kind !== "foreign") {
        return false;
      }
      if (onlyUnused && entry.facts.usage.state !== "never-since-install") {
        return false;
      }
      return true;
    })
    .sort((left, right) => {
      const key = SORTS[sortKey];
      const a = key(left);
      const b = key(right);
      return a < b ? -1 : a > b ? 1 : 0;
    });

  const shown = rows.slice(0, 500);

  document.querySelector("#packages tbody").innerHTML = shown
    .map((entry) => {
      const pkg = entry.package;
      const facts = entry.facts;
      const unused = facts.usage.state === "never-since-install" ? ' class="unused"' : "";
      return `<tr>
        <td class="name" title="${escapeHtml(pkg.description)}">${escapeHtml(pkg.name)}</td>
        <td class="num">${bytes(pkg.size)}</td>
        <td class="num">${bytes(facts.reclaimable)}</td>
        <td>${escapeHtml(pkg.reason)}</td>
        <td>${escapeHtml(originLabel(facts.origin))}</td>
        <td${unused}>${escapeHtml(lastUse(facts.usage))}</td>
      </tr>`;
    })
    .join("");

  for (const heading of document.querySelectorAll("th[data-sort]")) {
    heading.classList.toggle("sorted", heading.dataset.sort === sortKey);
  }

  const capped = rows.length > shown.length ? ` (first ${shown.length} listed)` : "";
  document.getElementById("status").textContent =
    `${rows.length} of ${report.inventory.entries.length} packages${capped}`;
}

async function load() {
  const button = document.getElementById("rescan");
  button.disabled = true;
  document.getElementById("status").textContent = "scanning…";

  try {
    const answer = await fetch("/api/inventory", { cache: "no-store" });
    if (!answer.ok) {
      throw new Error(`${answer.status} ${answer.statusText}: ${await answer.text()}`);
    }
    report = await answer.json();
  } catch (error) {
    document.getElementById("status").textContent = `scan failed: ${error.message}`;
    return;
  } finally {
    button.disabled = false;
  }

  renderSummary(report.summary);

  const caveat = document.getElementById("caveat");
  const support = report.inventory.atime_support;
  const frozen = support === "disabled" || support === "unknown";
  caveat.hidden = !frozen;
  if (frozen) {
    caveat.textContent =
      "access times are frozen on this system, so most packages cannot be dated — run `pacpurge --diagnose` for what that means here";
  }

  renderTable();
}

for (const id of ["search", "only-orphans", "only-foreign", "only-unused"]) {
  document.getElementById(id).addEventListener("input", renderTable);
}

for (const heading of document.querySelectorAll("th[data-sort]")) {
  heading.addEventListener("click", () => {
    sortKey = heading.dataset.sort;
    renderTable();
  });
}

document.getElementById("rescan").addEventListener("click", load);

load();
