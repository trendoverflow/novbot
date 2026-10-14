import { Navigate, Route, Routes } from 'react-router-dom'
import { AppLayout } from './layout/AppLayout'
import { OverviewPage } from './pages/OverviewPage'
import { NodesListPage } from './pages/NodesListPage'
import {
  NodeConfigTab,
  NodeDetailPage,
  NodeDispatchTab,
  NodeInventoryTab,
  NodeResultsTab,
  NodeSchedulesTab,
  NodeSkillsTab,
} from './pages/NodeDetailPage'
import { NodeTagsTab } from './pages/NodeTagsTab'
import { TagsPage } from './pages/TagsPage'
import { ResultsPage } from './pages/ResultsPage'
import { DispatchPage } from './pages/DispatchPage'
import { FleetSkillGroupsPage } from './pages/FleetSkillGroupsPage'
import { SchedulesPage } from './pages/SchedulesPage'
import { SkillDetailPage } from './pages/SkillDetailPage'
import { SkillsListPage } from './pages/SkillsListPage'
import {
  SettingsApiPage,
  SettingsIndexRedirect,
  SettingsLayout,
} from './pages/SettingsPage'

export default function App() {
  return (
    <Routes>
      <Route element={<AppLayout />}>
        <Route index element={<OverviewPage />} />
        <Route path="nodes" element={<NodesListPage />} />
        <Route path="tags" element={<TagsPage />} />
        <Route path="nodes/:nodeId" element={<NodeDetailPage />}>
          <Route index element={<Navigate to="inventory" replace />} />
          <Route path="inventory" element={<NodeInventoryTab />} />
          <Route path="tags" element={<NodeTagsTab />} />
          <Route path="skills" element={<NodeSkillsTab />} />
          <Route path="config" element={<NodeConfigTab />} />
          <Route path="schedules" element={<NodeSchedulesTab />} />
          <Route path="dispatch" element={<NodeDispatchTab />} />
          <Route path="results" element={<NodeResultsTab />} />
        </Route>
        <Route path="dispatch" element={<DispatchPage />} />
        <Route path="schedules" element={<SchedulesPage />} />
        <Route path="results" element={<ResultsPage />} />
        <Route path="skills" element={<SkillsListPage />} />
        <Route path="skills/:name" element={<SkillDetailPage />} />
        <Route path="fleet/skill-groups" element={<FleetSkillGroupsPage />} />
        <Route path="settings" element={<SettingsLayout />}>
          <Route index element={<SettingsIndexRedirect />} />
          <Route path="api" element={<SettingsApiPage />} />
        </Route>
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}
