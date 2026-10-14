import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { Scene } from '#lib/generated/Scene';
import SceneSummary from './SceneSummary.svelte';

const scene: Scene = {
	id: 'scene-1',
	name: 'Intro',
	position: { x: 10, y: 20 },
	layers: [
		{
			id: 'layer-1',
			name: 'Camera',
			kind: { type: 'camera' },
			rect: { x: 0, y: 0, w: 1920, h: 1080 },
			z: 0,
			crop: null
		},
		{
			id: 'layer-2',
			name: 'Title',
			kind: { type: 'text', text: 'Hello' },
			rect: { x: 96, y: 880, w: 1200, h: 120 },
			z: 1,
			crop: null
		}
	]
};

describe('SceneSummary', () => {
	it('renders the scene name and one row per layer', () => {
		render(SceneSummary, { scene });

		expect(screen.getByTestId('scene-name')).toHaveTextContent('Intro');
		expect(screen.getAllByTestId('layer')).toHaveLength(2);
		expect(screen.getByText(/Text "Hello"/)).toBeInTheDocument();
	});
});
