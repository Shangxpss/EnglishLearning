import React from 'react'

export const StoryViewer: React.FC<{text: string}> = ({text}) => {
  return (
    <section>
      <h3>Generated Story</h3>
      <div>{text}</div>
    </section>
  )
}
