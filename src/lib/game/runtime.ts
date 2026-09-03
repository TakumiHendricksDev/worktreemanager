/** Imperative Three.js lifecycle behind the declarative Svelte game surface. */

import * as THREE from 'three';

import type { GameSnapshot } from './model';
import {
  CameraRig,
  Colony,
  Engine,
  Settings,
  crewRig,
  loadCrew,
  loadKit,
  type RenderAgent,
  type RenderCamera,
  type RenderColony,
  type RenderEngine,
  type RenderSettings,
} from './vendor';

const LAYOUT_KEY = 'wtm.game.world-layout.v1';

export type WorldPick =
  | { kind: 'actor'; id: string }
  | { kind: 'job'; id: string }
  | { kind: 'repository'; id: string }
  | { kind: 'ground' };

export interface WorldRuntimeCallbacks {
  onpick(pick: WorldPick): void;
  onready(): void;
  onerror(message: string): void;
  onstats(stats: WorldStats): void;
}

export interface WorldStats {
  repositories: number;
  agents: number;
  working: number;
  waiting: number;
  blocked: number;
  fps: number;
}

export class WorldRuntime {
  readonly settings: RenderSettings;

  private readonly engine: RenderEngine;
  private readonly camera: RenderCamera;
  private readonly colony: RenderColony;
  private readonly callbacks: WorldRuntimeCallbacks;
  private readonly hoverGround = new THREE.Vector3();
  private readonly offSettings: () => void;
  private selectedActor: string | null = null;
  private lastLayout = '';
  private lastStats = '';
  private snapshot: GameSnapshot = { repositories: [] };

  constructor(host: HTMLElement, callbacks: WorldRuntimeCallbacks) {
    this.callbacks = callbacks;
    this.settings = new Settings();
    this.engine = new Engine(this.settings).mount(host);
    this.camera = new CameraRig(this.engine.camera, this.engine.canvas, this.settings);
    this.colony = new Colony(
      this.engine.scene,
      this.settings,
      this.engine.camera,
      this.engine.renderer,
    );
    this.restoreLayout();

    this.offSettings = this.settings.onChange((changed, scope) => {
      if (scope.render || changed.has('fov')) this.engine.applySettings();
      this.colony.onSettingsChanged(changed, scope);
      if (changed.has('maxAgents')) this.setSnapshot(this.snapshot);
    });

    this.engine.add({
      update: (delta, elapsed) => {
        this.camera.update(delta);
        this.colony.update(delta, elapsed, this.camera.target);
        this.engine.setFocusDistance(this.camera.distance);
        this.reportStats();
      },
    });

    this.engine.canvas.addEventListener('pointermove', this.onPointerMove);
    this.engine.canvas.addEventListener('pointerleave', this.onPointerLeave);
    this.engine.canvas.addEventListener('pointerup', this.onPointerUp);
    this.engine.start();
    void this.loadAssets();
  }

  setSnapshot(snapshot: GameSnapshot): void {
    this.snapshot = snapshot;
    this.colony.setWorld(snapshot);
    const layout = JSON.stringify(this.colony.layoutForSave());
    if (layout !== this.lastLayout) {
      this.lastLayout = layout;
      try {
        localStorage.setItem(LAYOUT_KEY, layout);
      } catch {
        // A stable map is a convenience. The world remains usable when storage is unavailable.
      }
    }
    if (this.selectedActor && !this.colony.agentFor(this.selectedActor)) {
      this.selectActor(null);
    }
    this.reportStats(true);
  }

  setVisible(visible: boolean): void {
    if (visible) {
      this.engine.resize();
      this.engine.start();
      this.engine.renderFrame();
    } else {
      this.engine.stop();
    }
  }

  selectActor(id: string | null): void {
    this.selectedActor = id;
    this.colony.astronauts.setSelected(id ? this.colony.agentFor(id) : null);
  }

  focusRepository(id: string): void {
    const plot = this.colony.plots.get(id);
    if (plot) this.camera.focus(plot.middle ?? plot.center, { distance: 30 });
  }

  focusJob(id: string): void {
    const entry = this.colony.buildings.get(id);
    if (entry) this.camera.focus(entry.mesh.position, { distance: 20 });
  }

  focusActor(id: string): void {
    const actor = this.colony.agentFor(id);
    if (!actor) return;
    this.selectActor(id);
    this.camera.focus(actor.pos, { distance: Math.min(this.camera.desiredDistance, 18) });
  }

  resetView(): void {
    this.camera.resetView();
  }

  toggleOrbit(): boolean {
    return this.camera.toggleOrbit();
  }

  screenshot(): void {
    this.engine.renderFrame();
    const link = document.createElement('a');
    link.href = this.engine.canvas.toDataURL('image/png');
    link.download = `wtm-world-${new Date().toISOString().replaceAll(':', '-').slice(0, 19)}.png`;
    link.click();
  }

  dispose(): void {
    this.engine.canvas.removeEventListener('pointermove', this.onPointerMove);
    this.engine.canvas.removeEventListener('pointerleave', this.onPointerLeave);
    this.engine.canvas.removeEventListener('pointerup', this.onPointerUp);
    this.offSettings();
    this.camera.dispose();
    this.colony.dispose();
    this.engine.dispose();
  }

  private readonly onPointerMove = (event: PointerEvent): void => {
    if (this.camera.interacting) {
      this.engine.canvas.style.cursor = 'grabbing';
      return;
    }
    const point = this.ndc(event);
    if (!point) return;
    const actor = this.colony.pick(point.x, point.y, point.aspect);
    this.colony.astronauts.setHover(actor);
    const plot = this.plotUnder(event, point);
    this.colony.setHoveredPlot(plot);
    const job = this.colony.pickJob(point.x, point.y);
    this.engine.canvas.style.cursor = actor || job || plot ? 'pointer' : 'grab';
  };

  private readonly onPointerLeave = (): void => {
    this.colony.astronauts.setHover(null);
    this.colony.setHoveredPlot(null);
  };

  private readonly onPointerUp = (event: PointerEvent): void => {
    if (event.button !== 0 || !this.camera.wasClick) return;
    const point = this.ndc(event);
    if (!point) return;
    const actor = this.colony.pick(point.x, point.y, point.aspect);
    if (actor) {
      this.selectActor(actor.id);
      this.callbacks.onpick({ kind: 'actor', id: actor.id });
      return;
    }
    const job = this.colony.pickJob(point.x, point.y);
    if (job) {
      this.selectActor(null);
      this.callbacks.onpick({ kind: 'job', id: job });
      return;
    }
    const plot = this.plotUnder(event, point);
    this.selectActor(null);
    this.callbacks.onpick(plot ? { kind: 'repository', id: plot.id } : { kind: 'ground' });
  };

  private ndc(event: PointerEvent): { x: number; y: number; aspect: number } | null {
    const rect = this.engine.canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return null;
    return {
      x: ((event.clientX - rect.left) / rect.width) * 2 - 1,
      y: -((event.clientY - rect.top) / rect.height) * 2 + 1,
      aspect: rect.width / rect.height,
    };
  }

  private plotUnder(
    event: PointerEvent,
    point: { x: number; y: number },
  ): { id: string } | null {
    const label = this.colony.pickLabel(point.x, point.y);
    if (label) return label;
    const ground = this.camera.groundPoint(event.clientX, event.clientY, this.hoverGround);
    return ground ? this.colony.plotAt(ground.x, ground.z) : null;
  }

  private restoreLayout(): void {
    try {
      const saved = localStorage.getItem(LAYOUT_KEY);
      if (saved) {
        this.lastLayout = saved;
        this.colony.restoreLayout(JSON.parse(saved));
      }
    } catch {
      // A corrupt layout should forget positions, never prevent the world from opening.
    }
  }

  private async loadAssets(): Promise<void> {
    const [kit, crew] = await Promise.allSettled([loadKit(), loadCrew()]);
    if (crew.status === 'fulfilled') this.colony.astronauts.setRig(crewRig());
    if (kit.status === 'fulfilled') this.colony.onAssetsReady();
    if (kit.status === 'rejected' || crew.status === 'rejected') {
      this.callbacks.onerror(
        'Some game assets could not be loaded. The standard interface is still available.',
      );
    }
    this.setSnapshot(this.snapshot);
    this.callbacks.onready();
  }

  private reportStats(force = false): void {
    const actors = this.snapshot.repositories.flatMap((repo) =>
      repo.jobs.flatMap((job) => job.actors),
    );
    const next: WorldStats = {
      repositories: this.snapshot.repositories.length,
      agents: actors.length,
      working: actors.filter((actor) => actor.gameStatus === 'working').length,
      waiting: actors.filter((actor) => actor.gameStatus === 'waiting').length,
      blocked: actors.filter((actor) => actor.gameStatus === 'blocked').length,
      fps: Math.round(this.engine.perf.fps || 0),
    };
    const signature = JSON.stringify(next);
    if (force || signature !== this.lastStats) {
      this.lastStats = signature;
      this.callbacks.onstats(next);
    }
  }
}
