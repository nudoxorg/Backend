<template>
  <span class="badge" :class="{ 'badge-new': isNew }">
    <slot />
  </span>
</template>

<script setup lang="ts">
defineProps<{ isNew?: boolean }>();
</script>

<style scoped>
.badge {
  display: inline-block;
  padding: 0 var(--space-3);
}
.badge-new {
  color: var(--brand);
}
</style>

<style>
.ribbon {
  position: absolute;
}
</style>
