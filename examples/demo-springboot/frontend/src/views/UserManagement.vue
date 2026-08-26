<template>
  <div class="user-management">
    <h1>用户管理列表</h1>
    <el-table :data="users">
      <el-table-column prop="id" label="ID" width="80" />
      <el-table-column prop="username" label="用户名" />
      <el-table-column prop="email" label="邮箱" />
    </el-table>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import axios from 'axios'

interface UserItem {
  id: number
  username: string
  email: string
}

const users = ref<UserItem[]>([])

const fetchUsers = async () => {
  const res = await axios.get('/api/users/list')
  users.value = res.data
}

onMounted(() => {
  fetchUsers()
})
</script>
