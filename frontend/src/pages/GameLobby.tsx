import React, { useEffect, useState } from 'react';
import { ApiClient } from '../services/typed-api-sdk';
import { Game } from '../types/api-client';
import StatusCard from '../components/v1/StatusCard';

const GameLobby: React.FC = () => {
  const [games, setGames] = useState<Game[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const fetchGames = async () => {
      const client = new ApiClient();
      const result = await client.getGames();
      
      if (result.success) {
        setGames(result.data);
      } else {
        setError(result.error.message);
      }
      setLoading(false);
    };

    fetchGames();
  }, []);

  if (loading) return <div className="lobby-loading">Loading elite games...</div>;
  if (error) return <div className="lobby-error">Failed to load games: {error}</div>;

  return (
    <div className="game-lobby">
      <div className="lobby-header">
        <h2>Live Arena</h2>
        <p>Real-time game status across the Stellar ecosystem.</p>
      </div>
      
      {games.length === 0 ? (
        <div className="lobby-empty">
          <div className="empty-icon">📭</div>
          <p>No games active at the moment. Check back later!</p>
        </div>
      ) : (
        <div className="games-grid">
          {games.map((game) => (
            <StatusCard 
              key={game.id}
              id={game.id}
              name={game.name}
              status={game.status}
              wager={game.wager as number | undefined}
            />
          ))}
        </div>
      )}
    </div>
  );
};

export default GameLobby;
