import { render, screen, waitFor } from '@testing-library/react';
import { expect, test, vi } from 'vitest';
import GameLobby from '../../src/pages/GameLobby';
import { ApiClient } from '../../src/services/typed-api-sdk';

vi.mock('../../src/services/typed-api-sdk');

test('renders GameLobby and fetches games', async () => {
  const mockGames = [
    { id: '123456789', name: 'Elite Clash', status: 'active', wager: 50 }
  ];
  
  (ApiClient as any).prototype.getGames.mockResolvedValue({
    success: true,
    data: mockGames
  });

  render(<GameLobby />);
  
  expect(screen.getByText(/Loading elite games.../i)).toBeDefined();
  
  await waitFor(() => {
    expect(screen.getByText(/Elite Clash/i)).toBeDefined();
    expect(screen.getByText(/50 XLM/i)).toBeDefined();
    expect(screen.getByText(/#12345678/i)).toBeDefined();
  });
});
