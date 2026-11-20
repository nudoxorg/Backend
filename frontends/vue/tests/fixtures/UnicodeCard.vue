<!-- π 🚀 host prefix -->
<template>
  <button :class="[$style.button, { active: enabled }]">{{ label }}</button>
</template>
<script setup lang="ts">
import { ref } from "vue";
const label = ref("π");
const enabled = ref(true);
</script>
<style module>
.button { color: var(--brand, blue); }
</style>
<style scoped lang="scss">
@mixin direction { [dir="rtl"] & { padding-left: 0; } }
.nav { @include direction; }
</style>
