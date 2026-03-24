import React from 'react';

interface StatusCardProps {
  id: string;
  name: string;
  status: string;
  wager?: number;
}

const StatusCard: React.FC<StatusCardProps> = ({ id, name, status, wager }) => {
  const getStatusColor = (s: string) => {
    switch (s.toLowerCase()) {
      case 'active': return '#00ffcc';
      case 'pending': return '#ffcc00';
      case 'completed': return '#888888';
      default: return '#ffffff';
    }
  };

  return (
    <div className="status-card">
      <div className="status-indicator" style={{ backgroundColor: getStatusColor(status) }}></div>
      <div className="card-header">
        <h3>{name}</h3>
        <span className="game-id">#{id.slice(0, 8)}</span>
      </div>
      <div className="card-body">
        <div className="status-label">{status.toUpperCase()}</div>
        {wager && <div className="wager-amount">{wager} XLM</div>}
      </div>
      <div className="card-footer">
        <button className="btn-play">Join Game</button>
      </div>
    </div>
  );
};

export default StatusCard;
