import { useState } from 'react'
import './App.css'
import { Button } from './components/ui/button'


function App() {
  const [count, setCount] = useState(0)

  return (
    <>
      <Button variant={'link'} onClick={() => setCount(count + 1)}>Click me</Button>
      <div className='bg-amber-500'>
        {count}
      </div>
    </>
  )
}

export default App
