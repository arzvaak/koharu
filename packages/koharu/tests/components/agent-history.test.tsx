import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { AgentPanel } from '@/components/editor/AgentPanel'

const status = vi.hoisted(() => ({
  account: { id: 'account', email: 'reader@example.com', plan: 'plus' },
  models: [],
  config: { model: null, reasoning: 'medium' },
  running: null,
  active_chat: 'saved',
  chats: [
    {
      id: 'saved',
      title: 'Earlier translation',
      updated_at: '1',
      messages: [
        { role: 'user', text: 'Remember Elser', error: false },
        { role: 'assistant', text: 'I will use Elser.', error: false },
      ],
    },
  ],
}))
const getStatus = vi.hoisted(() => vi.fn(async () => status))
const selectChat = vi.hoisted(() =>
  vi.fn(async () => ({
    ...status,
    active_chat: 'new',
    chats: [...status.chats, { id: 'new', title: 'New chat', updated_at: '2', messages: [] }],
  })),
)
vi.mock('@koharu/bridge/protocol', () => ({
  commands: { getAgentStatus: getStatus, selectAgentChat: selectChat },
}))
vi.mock('@/lib/backend', () => ({
  call: (fn: (...args: unknown[]) => unknown, ...args: unknown[]) => fn(...args),
}))
vi.mock('@/lib/queries', () => ({
  useProject: () => ({ data: { name: 'Protecia' } }),
  refresh: vi.fn(),
}))

describe('saved Agent chats', () => {
  it('restores the selected transcript after remount and starts a separate new chat', async () => {
    const first = render(<AgentPanel />)
    expect(await screen.findByText('I will use Elser.')).toBeInTheDocument()
    first.unmount()
    render(<AgentPanel />)
    expect(await screen.findByText('Remember Elser')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    expect(await screen.findByText('What should Koharu do?')).toBeInTheDocument()
    expect(selectChat).toHaveBeenCalledWith(null)
    expect(screen.queryByText('Remember Elser')).not.toBeInTheDocument()
  })
})
