/**
 * Typed declarations for the isolated Bot Crossing renderer.
 *
 * The upstream implementation remains JavaScript so it can be compared with the pinned source.
 * All unchecked values stop here; WTM components only see the contracts below.
 */

import type * as THREE from 'three';
import type { GameSnapshot } from './model';

// @ts-expect-error The pinned upstream module is intentionally vendored as JavaScript.
import { Engine as RawEngine } from './vendor/core/engine.js';
// @ts-expect-error The pinned upstream module is intentionally vendored as JavaScript.
import { CameraRig as RawCameraRig } from './vendor/core/camera.js';
// @ts-expect-error The pinned upstream module is intentionally vendored as JavaScript.
import { Settings as RawSettings, PRESETS as RAW_PRESETS } from './vendor/core/settings.js';
// @ts-expect-error The pinned upstream module is intentionally vendored as JavaScript.
import { Colony as RawColony } from './vendor/game/colony.js';
// @ts-expect-error The pinned upstream module is intentionally vendored as JavaScript.
import { loadKit as rawLoadKit } from './vendor/world/kit.js';
// @ts-expect-error The pinned upstream module is intentionally vendored as JavaScript.
import { crewRig as rawCrewRig, loadCrew as rawLoadCrew } from './vendor/agents/crew.js';

export interface RenderSettings {
  values: Record<string, unknown>;
  get(key: string): any;
  set(key: string, value: unknown): void;
  applyPreset(name: string): void;
  onChange(
    listener: (changed: Set<string>, scope: { world: boolean; render: boolean }) => void,
  ): () => void;
}

export interface RenderEngine {
  scene: THREE.Scene;
  camera: THREE.PerspectiveCamera;
  renderer: THREE.WebGLRenderer;
  canvas: HTMLCanvasElement;
  viewport?: { w: number; h: number; bw: number; bh: number; scale: number };
  perf: { fps: number; frameMs: number; drawCalls: number; triangles: number };
  autoScaled?: boolean;
  mount(parent: HTMLElement): this;
  add(updater: { update(delta: number, elapsed: number): void }): unknown;
  applySettings(): void;
  setFocusDistance(distance: number): void;
  renderFrame(): void;
  resize(): void;
  start(): void;
  stop(): void;
  dispose(): void;
}

export interface RenderCamera {
  interacting: boolean;
  wasClick: boolean;
  distance: number;
  desiredDistance: number;
  target: THREE.Vector3;
  update(delta: number): void;
  focus(point: THREE.Vector3, options?: { distance?: number }): void;
  resetView(): void;
  toggleOrbit(): boolean;
  groundPoint(clientX: number, clientY: number, out?: THREE.Vector3): THREE.Vector3 | null;
  dispose(): void;
}

export interface RenderAgent {
  id: string;
  pos: THREE.Vector3;
  thread: { id: string; worktreeId: string; projectId: string };
}

export interface RenderColony {
  plots: Map<string, { middle?: THREE.Vector3; center: THREE.Vector3 }>;
  buildings: Map<string, { mesh: THREE.Object3D }>;
  astronauts: {
    agents: RenderAgent[];
    setSelected(agent: RenderAgent | null): void;
    setHover(agent: RenderAgent | null): void;
    setRig(rig: unknown): void;
    celebrate(id: string): void;
    visibleCount: number;
  };
  particles: { liveCount: number };
  setWorld(snapshot: GameSnapshot): Record<string, number>;
  restoreLayout(saved: unknown): void;
  layoutForSave(): unknown;
  onAssetsReady(): void;
  onSettingsChanged(changed: Set<string>, scope: { world: boolean; render: boolean }): void;
  update(delta: number, elapsed: number, focus: THREE.Vector3): void;
  pick(x: number, y: number, aspect: number): RenderAgent | null;
  pickJob(x: number, y: number): string | null;
  pickLabel(x: number, y: number): { id: string } | null;
  plotAt(x: number, z: number): { id: string } | null;
  setHoveredPlot(plot: { id: string } | null): void;
  agentFor(id: string): RenderAgent | null;
  dispose(): void;
}

export const Engine = RawEngine as new (settings: RenderSettings) => RenderEngine;
export const CameraRig = RawCameraRig as new (
  camera: THREE.PerspectiveCamera,
  canvas: HTMLCanvasElement,
  settings: RenderSettings,
) => RenderCamera;
export const Settings = RawSettings as new () => RenderSettings;
export const Colony = RawColony as new (
  scene: THREE.Scene,
  settings: RenderSettings,
  camera: THREE.PerspectiveCamera,
  renderer: THREE.WebGLRenderer,
) => RenderColony;

export const PRESETS = RAW_PRESETS as Record<
  string,
  { label: string; hint: string; values: Record<string, unknown> }
>;
export const loadKit = rawLoadKit as () => Promise<unknown>;
export const loadCrew = rawLoadCrew as () => Promise<unknown>;
export const crewRig = rawCrewRig as () => unknown;
