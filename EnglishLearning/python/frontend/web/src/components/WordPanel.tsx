import React from 'react'

export const WordPanel: React.FC<{words: string[]}> = ({words}) => {
  return (
    <aside>
      <h3>Collected Words</h3>
      <ul>
        {words.map(w => <li key={w}>{w}</li>)}
      </ul>
    </aside>
  )
}
