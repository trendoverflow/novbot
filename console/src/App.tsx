import { Navigate, Route, Routes } from 'react-router-dom'
import { AppLayout } from './layout/AppLayout'
import { OverviewPage } from './pages/OverviewPage'
import { NodesListPage } from './pages/NodesListPage'
import {
  NodeConfigTab,
  NodeDetailPage,
  NodeDispatchTab,
  NodeResultsTab,
  NodeSchedulesTab,
} from './pages/NodeDetailPage'
import { ResultsPage } from './pages/ResultsPage'

export default function App() {
  return (
    <Routes>
      <Route element={<AppLayout />}>
        <Route index element={<OverviewPage />} />
        <Route path="nodes" element={<NodesListPage />} />
        <Route path="nodes/:nodeId" element={<NodeDetailPage />}>
          <Route index element={<Navigate to="config" replace />} />
          <Route path="config" element={<NodeConfigTab />} />
          <Route path="schedules" element={<NodeSchedulesTab />} />
          <Route path="dispatch" element={<NodeDispatchTab />} />
          <Route path="results" element={<NodeResultsTab />} />
        </Route>
        <Route path="results" element={<ResultsPage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}
